//! An effect's WGSL: the generated `Params` header, validation with naga against the
//! host's prelude, and the uniform bytes for a settings map.
//!
//! The contract with the host (the filter harness in `jstream-render`): the host
//! prepends its prelude, which declares the vertex entry point `vs`, the `VsOut` it
//! returns, its bindings and colour helpers. This crate appends the `Params` struct (one
//! field per parameter, in declaration order) bound at group [`PARAMS_GROUP`], binding
//! [`PARAMS_BINDING`] as `p`, then the effect's own file. That file defines
//! `@fragment fn fs(in: VsOut) -> @location(0) vec4<f32>` and nothing the host has to
//! bind: no bindings, no immediates, no overrides, no other entry points. Its memory is
//! bounded too ([`MAX_VALUE_BYTES`]), since a huge array is a cheap way to make a driver
//! spill to scratch memory or stall compiling it (#349).

use std::collections::BTreeSet;
use std::error::Error as _;

use jstream_protocol::{ParamDescriptor, ParamType, Settings};
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Binding, Module, ScalarKind, ShaderStage, TypeInner, VectorSize};

/// The bind group of the `Params` uniform.
pub const PARAMS_GROUP: u32 = 0;

/// The binding of the `Params` uniform.
pub const PARAMS_BINDING: u32 = 4;

/// The fragment entry point an effect defines.
pub const ENTRY_POINT: &str = "fs";

/// The most bytes an effect may hold per invocation in each place: any one value (a type
/// it names, constructs or declares), all its `var<private>` globals together, and the
/// local variables of any one function together. 16 KiB fits lookup tables of a few
/// hundred colours; a fragment shader needing more is a GPU hazard, not an effect.
pub const MAX_VALUE_BYTES: u32 = 16 * 1024;

/// One `Params` field's WGSL type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    /// `u32`: a `bool` (0 or 1) or an `enum` (the option's index in declaration order).
    U32,
    /// `i32`: an `int`.
    I32,
    /// `f32`: a `float`.
    F32,
    /// `vec4<f32>`: a `color` as linear-light, straight-alpha RGBA (alpha 1 without one).
    Vec4,
}

impl FieldType {
    fn of(ty: &ParamType) -> Self {
        match ty {
            ParamType::Int { .. } => Self::I32,
            ParamType::Float { .. } => Self::F32,
            ParamType::Color { .. } => Self::Vec4,
            // Bool and Enum; the manifest admits no other type.
            _ => Self::U32,
        }
    }

    fn wgsl(self) -> &'static str {
        match self {
            Self::U32 => "u32",
            Self::I32 => "i32",
            Self::F32 => "f32",
            Self::Vec4 => "vec4<f32>",
        }
    }

    /// WGSL's uniform alignment, which is also the size but for `Vec4`.
    fn align(self) -> usize {
        match self {
            Self::Vec4 => 16,
            _ => 4,
        }
    }

    fn size(self) -> usize {
        match self {
            Self::Vec4 => 16,
            _ => 4,
        }
    }
}

/// Where each parameter sits in the `Params` uniform.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    fields: Vec<(String, FieldType, usize)>,
    size: usize,
}

impl Layout {
    pub(crate) fn new(params: &[ParamDescriptor]) -> Self {
        let mut offset: usize = 0;
        let mut fields = Vec::with_capacity(params.len());
        for param in params {
            let ty = FieldType::of(&param.ty);
            offset = offset.next_multiple_of(ty.align());
            fields.push((param.id.clone(), ty, offset));
            offset += ty.size();
        }
        // Padded to 16, and never empty, so the host can always bind a buffer.
        Self {
            fields,
            size: offset.next_multiple_of(16).max(16),
        }
    }

    /// Each field's parameter ID, type and byte offset, in declaration order.
    pub fn fields(&self) -> impl ExactSizeIterator<Item = (&str, FieldType, usize)> {
        self.fields
            .iter()
            .map(|(id, ty, at)| (id.as_str(), *ty, *at))
    }

    /// The uniform's size in bytes: a multiple of 16, at least 16.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The WGSL declaring `Params` and `p`; empty when there are no parameters.
    pub(crate) fn header(&self) -> String {
        self.header_at(PARAMS_BINDING)
    }

    /// [`Self::header`] with `p` at `binding` of group 0.
    pub(crate) fn header_at(&self, binding: u32) -> String {
        if self.fields.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "// Generated from effect.toml: one field per parameter, in declaration order.\nstruct Params {\n",
        );
        for (id, ty, _) in &self.fields {
            out.push_str(&format!("    {id}: {},\n", ty.wgsl()));
        }
        out.push_str(&format!(
            "}}\n@group({PARAMS_GROUP}) @binding({binding}) var<uniform> p: Params;\n"
        ));
        out
    }

    /// The uniform's bytes for `settings`, complete as the graph stores them. A missing or
    /// mistyped value reads as the parameter's default; an out-of-range one is clamped to
    /// what the field holds.
    pub(crate) fn write(&self, params: &[ParamDescriptor], settings: &Settings) -> Vec<u8> {
        let mut bytes = vec![0; self.size];
        for (param, (_, ty, at)) in params.iter().zip(&self.fields) {
            let value = settings
                .0
                .get(&param.id)
                .filter(|v| param.check(v).is_ok())
                .unwrap_or(&param.default);
            let mut put = |at: usize, word: [u8; 4]| bytes[at..at + 4].copy_from_slice(&word);
            match (ty, &param.ty) {
                (FieldType::U32, ParamType::Enum { options }) => {
                    let index = value
                        .as_str()
                        .and_then(|v| options.iter().position(|o| o.value == v))
                        .unwrap_or(0);
                    put(*at, u32::try_from(index).unwrap_or(0).to_le_bytes());
                }
                (FieldType::U32, _) => {
                    put(
                        *at,
                        u32::from(value.as_bool().unwrap_or(false)).to_le_bytes(),
                    );
                }
                (FieldType::I32, _) => {
                    let v = value.as_i64().unwrap_or(0);
                    let v = i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
                    put(*at, v.to_le_bytes());
                }
                (FieldType::F32, _) => {
                    let v = value.as_f64().unwrap_or(0.0);
                    let v = v.clamp(f64::from(f32::MIN), f64::from(f32::MAX)) as f32;
                    put(*at, v.to_le_bytes());
                }
                (FieldType::Vec4, _) => {
                    let rgba = value.as_str().and_then(linear_rgba).unwrap_or([0.0; 4]);
                    for (i, c) in rgba.into_iter().enumerate() {
                        put(at + 4 * i, c.to_le_bytes());
                    }
                }
            }
        }
        bytes
    }
}

/// `#rrggbb` or `#rrggbbaa` as linear-light RGB with straight alpha, as the built-in
/// filters read colours.
fn linear_rgba(text: &str) -> Option<[f32; 4]> {
    let hex = text.strip_prefix('#')?;
    if !hex.is_ascii() || (hex.len() != 6 && hex.len() != 8) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    let alpha = if hex.len() == 8 { byte(6)? } else { 255 };
    let linear = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    Some([
        linear(byte(0)?),
        linear(byte(2)?),
        linear(byte(4)?),
        f32::from(alpha) / 255.0,
    ])
}

/// Parses and validates `prelude`, then `header`, then `source`, and checks the effect
/// contract. The error is a message for the effect's file, with its own line numbers.
pub(crate) fn validate(prelude: &str, header: &str, source: &str) -> Result<(), String> {
    validate_parts(prelude, header, &[source]).map_err(|(_, message)| message)
}

/// [`validate`] for a shader-source pass, whose text is the pack's `common` file (when it
/// has one) then the pass's file: `parts`, in order. The error names the part it is in,
/// by index, with that part's own line numbers. The contract is the effect's, and the
/// header may declare `p` and the pass's input textures, which it then reads.
pub(crate) fn validate_parts(
    prelude: &str,
    header: &str,
    parts: &[&str],
) -> Result<(), (usize, String)> {
    let last = parts.len().saturating_sub(1);
    let host_error = |m: String| (last, m);
    let host = naga::front::wgsl::parse_str(prelude)
        .map_err(|e| host_error(format!("the host prelude does not parse: {}", e.message())))?;
    // A host mistake is named as one, not blamed on the generated header.
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&host)
        .map_err(|e| {
            host_error(format!(
                "the host prelude does not validate: {}",
                e.as_inner()
            ))
        })?;
    let head = format!("{prelude}\n{header}\n");
    // Each part's first line is the one after the text before it.
    let mut starts = Vec::with_capacity(parts.len());
    let mut full = head.clone();
    for part in parts {
        starts.push(full.matches('\n').count());
        full.push_str(part);
        full.push('\n');
    }
    let head_lines = head.matches('\n').count();
    let at = |location: Option<naga::SourceLocation>| -> (usize, String) {
        match location {
            Some(l) if l.line_number as usize > head_lines => {
                let line = l.line_number as usize;
                let part = starts.iter().rposition(|&s| line > s).unwrap_or(last);
                (part, format!("line {}: ", line - starts[part]))
            }
            Some(_) => (
                last,
                "in the generated `Params` or inputs (a parameter ID or name WGSL reserves?): "
                    .to_owned(),
            ),
            None => (last, String::new()),
        }
    };
    let module = naga::front::wgsl::parse_str(&full).map_err(|e| {
        let (part, prefix) = at(e.location(&full));
        (part, format!("{prefix}{}", e.message()))
    })?;
    Validator::new(ValidationFlags::all(), Capabilities::empty())
        .validate(&module)
        .map_err(|e| {
            let mut message = e.as_inner().to_string();
            let mut cause = e.as_inner().source();
            while let Some(c) = cause {
                message.push_str(": ");
                message.push_str(&c.to_string());
                cause = c.source();
            }
            let (part, prefix) = at(e.location(&full));
            (part, format!("{prefix}{message}"))
        })?;
    // What the header declares (`p`, a pass's inputs) belongs to the host.
    let declared = header_globals(prelude, header);
    check_contract(&host, &module, &declared).map_err(|m| (last, m))
}

/// The globals `header` declares after `prelude`, by name; empty when it declares none
/// or does not parse alone (the full text's parse then names the problem).
fn header_globals(prelude: &str, header: &str) -> BTreeSet<String> {
    if header.is_empty() {
        return BTreeSet::new();
    }
    naga::front::wgsl::parse_str(&format!("{prelude}\n{header}\n"))
        .map(|m| {
            m.global_variables
                .iter()
                .filter_map(|(_, g)| g.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn check_contract(
    host: &Module,
    module: &Module,
    declared: &BTreeSet<String>,
) -> Result<(), String> {
    let host_entries: BTreeSet<&str> = host.entry_points.iter().map(|e| e.name.as_str()).collect();
    let mut found = false;
    for entry in &module.entry_points {
        if host_entries.contains(entry.name.as_str()) {
            continue;
        }
        if entry.name != ENTRY_POINT || entry.stage != ShaderStage::Fragment {
            return Err(format!(
                "declares the entry point `{}`; an effect defines only `@fragment fn {ENTRY_POINT}`",
                entry.name
            ));
        }
        let returns_rgba = entry.function.result.as_ref().is_some_and(|r| {
            matches!(r.binding, Some(Binding::Location { location: 0, .. }))
                && matches!(
                    module.types[r.ty].inner,
                    TypeInner::Vector {
                        size: VectorSize::Quad,
                        scalar: naga::Scalar {
                            kind: ScalarKind::Float,
                            width: 4
                        }
                    }
                )
        });
        if !returns_rgba {
            return Err(format!(
                "`{ENTRY_POINT}` must return `@location(0) vec4<f32>`"
            ));
        }
        found = true;
    }
    if !found {
        return Err(format!(
            "defines no `@fragment fn {ENTRY_POINT}(in: VsOut) -> @location(0) vec4<f32>`"
        ));
    }
    if let Some((_, o)) = module.overrides.iter().next() {
        return Err(format!(
            "declares the override `{}`; an effect takes its values only from parameters",
            o.name.as_deref().unwrap_or("?")
        ));
    }
    let host_globals: BTreeSet<Option<&str>> = host
        .global_variables
        .iter()
        .map(|(_, g)| g.name.as_deref())
        .collect();
    for (_, global) in module.global_variables.iter() {
        let name = global.name.as_deref();
        if host_globals.contains(&name) {
            continue;
        }
        if name.is_some_and(|n| declared.contains(n)) || global.space == AddressSpace::Private {
            continue;
        }
        return Err(format!(
            "declares `{}` in the {:?} address space; an effect reads only the host's \
             bindings and `p`, and may add `var<private>` globals",
            name.unwrap_or("?"),
            global.space
        ));
    }
    check_memory(module)
}

/// Checks [`MAX_VALUE_BYTES`]: every type in the module (which covers what the effect
/// declares and what it constructs, such as `array<f32, N>()`), the `var<private>`
/// globals together, and each function's local variables together. The host prelude's
/// own types are small, so checking the whole module blames only the effect.
fn check_memory(module: &Module) -> Result<(), String> {
    let mut layouter = naga::proc::Layouter::default();
    layouter
        .update(module.to_ctx())
        .map_err(|e| format!("its types have no layout: {e}"))?;
    let size = |ty: naga::Handle<naga::Type>| u64::from(layouter[ty].size);
    let limit = u64::from(MAX_VALUE_BYTES);
    for (handle, ty) in module.types.iter() {
        if size(handle) > limit {
            return Err(format!(
                "uses a {}-byte value{}; an effect's values are at most {MAX_VALUE_BYTES} bytes",
                size(handle),
                ty.name
                    .as_deref()
                    .map(|n| format!(" (`{n}`)"))
                    .unwrap_or_default()
            ));
        }
    }
    let private: u64 = module
        .global_variables
        .iter()
        .filter(|(_, g)| g.space == AddressSpace::Private)
        .map(|(_, g)| size(g.ty))
        .sum();
    if private > limit {
        return Err(format!(
            "declares {private} bytes of `var<private>` globals; an effect holds at most \
             {MAX_VALUE_BYTES}"
        ));
    }
    let functions = module
        .functions
        .iter()
        .map(|(_, f)| f)
        .chain(module.entry_points.iter().map(|e| &e.function));
    for function in functions {
        let locals: u64 = function
            .local_variables
            .iter()
            .map(|(_, v)| size(v.ty))
            .sum();
        if locals > limit {
            return Err(format!(
                "`{}` declares {locals} bytes of local variables; a function holds at most \
                 {MAX_VALUE_BYTES}",
                function.name.as_deref().unwrap_or("?")
            ));
        }
    }
    Ok(())
}
