//! The H.264 decoding process around the slice data, for a hardware decoder that is told
//! everything (ITU-T H.264 §8.2, §C.4): picture order counts, reference marking, the
//! reference lists of each slice, and when each picture is output.
//!
//! [`Planner`] takes access units in decoding order and says, for each picture, which
//! buffer slot it decodes into, which slots it references and how, and which slots are
//! output, in presentation order. It knows nothing of VA-API: slots are indexes into the
//! caller's surfaces.
//!
//! Frames only: a stream with field pictures is refused. MBAFF frames are frames, and
//! decode.

use std::collections::HashMap;

use super::bits::{ParseError, Result, unescape};
use super::params::{Pps, Sps};
use super::slice::{ListModification, Mmco, SliceHeader, SliceType};

/// One frame in the decoded picture buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    /// The caller's buffer, or `None` for a "non-existing" frame standing in for a gap
    /// in `frame_num`.
    pub(crate) slot: Option<usize>,
    pub(crate) frame_num: u32,
    pub(crate) top_poc: i32,
    pub(crate) bottom_poc: i32,
    /// `LongTermFrameIdx`, for a long-term reference.
    pub(crate) long_term: Option<u32>,
    /// A short- or long-term reference.
    pub(crate) reference: bool,
    /// Still to be output.
    pub(crate) waiting: bool,
    /// The presentation time the container gave it.
    pub(crate) pts: i64,
}

impl Frame {
    /// `PicOrderCnt()` of a frame.
    pub(crate) fn poc(&self) -> i32 {
        self.top_poc.min(self.bottom_poc)
    }

    fn short_term(&self) -> bool {
        self.reference && self.long_term.is_none()
    }

    fn long_term_ref(&self) -> bool {
        self.reference && self.long_term.is_some()
    }
}

/// One slice to decode: its header, its NAL unit as stored (escaped, header byte
/// included), and its two reference lists as frames.
#[derive(Clone, Debug)]
pub(crate) struct SlicePlan {
    pub(crate) header: SliceHeader,
    pub(crate) nal: Vec<u8>,
    pub(crate) list0: Vec<Frame>,
    pub(crate) list1: Vec<Frame>,
}

/// One picture to decode.
#[derive(Clone, Debug)]
pub(crate) struct PicturePlan {
    /// The slot it decodes into.
    pub(crate) slot: usize,
    pub(crate) sps: Sps,
    pub(crate) pps: Pps,
    /// The picture itself, as VA-API's `CurrPic` describes it.
    pub(crate) current: Frame,
    /// Every reference frame in the buffer (VA-API's `ReferenceFrames`), real frames
    /// only.
    pub(crate) references: Vec<Frame>,
    pub(crate) slices: Vec<SlicePlan>,
}

/// A decoded frame to hand on, in presentation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Output {
    pub(crate) slot: usize,
    pub(crate) pts: i64,
}

/// What the previous pictures leave for the next one's picture order count (§8.2.1).
#[derive(Clone, Copy, Debug, Default)]
struct PocState {
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num_offset: i64,
    prev_frame_num: u32,
    /// `frame_num` of the previous reference picture, for gap detection.
    prev_ref_frame_num: u32,
}

/// The decoding process's state over a stream. See the module docs.
pub(crate) struct Planner {
    sps: HashMap<u8, Sps>,
    pps: HashMap<u8, Pps>,
    dpb: Vec<Frame>,
    poc: PocState,
    /// `MaxLongTermFrameIdx`, `None` for "no long-term frame indices".
    max_long_term_idx: Option<u32>,
    /// How many slots the caller has.
    slots: usize,
    /// The slot of the picture being decoded, between `submit` and `finish`.
    pending: Option<(Frame, SliceHeader, Sps)>,
    /// Whether decoding has a starting point yet: an IDR picture, or after a seek an
    /// all-intra one (an open GOP's recovery point). Pictures before it are skipped,
    /// since what they reference was never decoded.
    started: bool,
    /// After starting on an all-intra picture that isn't IDR: its picture order count.
    /// Pictures decoded after it but shown before it (the open GOP's leading pictures)
    /// reference frames before the starting point, so they are decoded but never
    /// output. Cleared by the next IDR picture or MMCO 5.
    start_poc: Option<i32>,
}

impl Planner {
    /// A planner for a caller with `slots` buffers.
    pub(crate) fn new(slots: usize) -> Self {
        Self {
            sps: HashMap::new(),
            pps: HashMap::new(),
            dpb: Vec::new(),
            poc: PocState::default(),
            max_long_term_idx: None,
            slots,
            pending: None,
            started: false,
            start_poc: None,
        }
    }

    /// The slots needed for `sps`: its buffer, the current picture, and one to spare.
    pub(crate) fn slots_for(sps: &Sps) -> usize {
        usize::try_from(sps.dpb_frames()).unwrap_or(16).min(16) + 2
    }

    /// Adds a parameter set from the container's decoder configuration or the stream.
    /// `nal` is escaped, header byte included.
    pub(crate) fn parameter_set(&mut self, nal: &[u8]) -> Result<()> {
        let unit = unescape(nal);
        let Some(&header) = unit.first() else {
            return Ok(());
        };
        match header & 0x1F {
            7 => {
                let sps = Sps::parse(&unit[1..])?;
                if !sps.frame_mbs_only {
                    // Field pictures may follow; refused when one arrives.
                    tracing::debug!("an interlaced H.264 sequence");
                }
                self.sps.insert(sps.id, sps);
            }
            8 => {
                let pps = Pps::parse(&unit[1..], |id| self.sps.get(&id).cloned())?;
                self.pps.insert(pps.id, pps);
            }
            _ => {}
        }
        Ok(())
    }

    /// The sequence parameter set to size the decoder for: the one the lowest-numbered
    /// picture parameter set names, else the lowest-numbered one.
    pub(crate) fn first_sps(&self) -> Option<&Sps> {
        let named = self
            .pps
            .iter()
            .min_by_key(|(id, _)| **id)
            .and_then(|(_, pps)| self.sps.get(&pps.sps_id));
        named.or_else(|| self.sps.iter().min_by_key(|(id, _)| **id).map(|(_, s)| s))
    }

    /// Forgets the decoded frames (a seek), keeping the parameter sets. Every frame
    /// still waiting is dropped, not output.
    pub(crate) fn reset(&mut self) {
        self.dpb.clear();
        self.poc = PocState::default();
        self.max_long_term_idx = None;
        self.pending = None;
        self.started = false;
        self.start_poc = None;
    }

    /// Takes one access unit's NAL units (escaped, header byte included) shown at `pts`.
    /// Returns the frames to output first, in order, and the picture to decode, if the
    /// unit has one. After decoding it, call [`finish`](Self::finish).
    ///
    /// # Errors
    /// [`ParseError`] for a unit that doesn't parse or uses a tool this doesn't support.
    pub(crate) fn submit(
        &mut self,
        nals: &[&[u8]],
        pts: i64,
    ) -> Result<(Vec<Output>, Option<PicturePlan>)> {
        let mut slices: Vec<(SliceHeader, Vec<u8>)> = Vec::new();
        let mut pps = None;
        let mut sps = None;
        for nal in nals {
            let Some(&first) = nal.first() else { continue };
            match first & 0x1F {
                7 | 8 => self.parameter_set(nal)?,
                1 | 5 => {
                    // The header is short: unescaping its first bytes is enough, but the
                    // unit is small next to the frame, so take the whole of it.
                    let unit = unescape(nal);
                    let (header, p, s) = SliceHeader::parse(
                        &unit,
                        |id| self.pps.get(&id).cloned(),
                        |id| self.sps.get(&id).cloned(),
                    )?;
                    if header.field_pic {
                        return Err(ParseError::Unsupported("field-coded (interlaced) H.264"));
                    }
                    if header.redundant_pic_cnt > 0 {
                        continue;
                    }
                    if pps.is_none() {
                        pps = Some(p);
                        sps = Some(s);
                    }
                    slices.push((header, nal.to_vec()));
                }
                // Data partitions (2 to 4) belong to the Extended profile.
                2..=4 => return Err(ParseError::Unsupported("data-partitioned H.264")),
                _ => {}
            }
        }
        let (Some(pps), Some(sps), Some((first, _))) = (pps, sps, slices.first()) else {
            return Ok((Vec::new(), None));
        };
        let first = first.clone();
        let mut recovery = false;
        if first.idr {
            self.started = true;
            self.start_poc = None;
        } else if !self.started {
            let intra = slices
                .iter()
                .all(|(h, _)| matches!(h.slice_type, SliceType::I | SliceType::Si));
            if !intra {
                return Ok((Vec::new(), None));
            }
            // A recovery point: decoding starts here with an empty buffer, its own
            // frame_num taken as the previous one so no gap is filled, and its picture
            // order count relative to itself.
            self.started = true;
            recovery = true;
            self.poc = PocState {
                prev_poc_msb: 0,
                prev_poc_lsb: i32::try_from(first.pic_order_cnt_lsb).unwrap_or(0),
                prev_frame_num_offset: 0,
                prev_frame_num: first.frame_num,
                prev_ref_frame_num: first.frame_num,
            };
        }
        if sps.chroma_format_idc != 1
            || sps.bit_depth_luma_minus8 != 0
            || sps.bit_depth_chroma_minus8 != 0
        {
            return Err(ParseError::Unsupported("H.264 other than 8-bit 4:2:0"));
        }

        let mut outputs = Vec::new();
        if first.idr {
            // Everything before an IDR picture is output, then forgotten.
            outputs.extend(self.drain());
            self.dpb.clear();
            self.poc = PocState::default();
        } else {
            self.fill_gap(&first, &sps);
        }
        let (top_poc, bottom_poc) = self.picture_order_count(&first, &sps);
        if recovery {
            self.start_poc = Some(top_poc.min(bottom_poc));
        }
        let leading = self
            .start_poc
            .is_some_and(|start| top_poc.min(bottom_poc) < start);

        let slot = self.free_slot().ok_or(ParseError::Invalid(
            "more frames held than the decoded picture buffer allows",
        ))?;
        let current = Frame {
            slot: Some(slot),
            frame_num: first.frame_num,
            top_poc,
            bottom_poc,
            long_term: None,
            reference: first.is_reference(),
            waiting: !leading,
            pts,
        };

        let mut plans = Vec::with_capacity(slices.len());
        for (header, nal) in slices {
            let (list0, list1) = self.reference_lists(&header, &sps, &current);
            plans.push(SlicePlan {
                header,
                nal,
                list0,
                list1,
            });
        }
        let references = self
            .dpb
            .iter()
            .filter(|f| f.reference && f.slot.is_some())
            .copied()
            .collect();
        self.pending = Some((current, first, sps.clone()));
        Ok((
            outputs,
            Some(PicturePlan {
                slot,
                sps,
                pps,
                current,
                references,
                slices: plans,
            }),
        ))
    }

    /// Ends the picture [`submit`](Self::submit) returned, once it is decoded: marks
    /// the references and returns the frames now ready to output.
    pub(crate) fn finish(&mut self) -> Vec<Output> {
        let Some((mut current, header, sps)) = self.pending.take() else {
            return Vec::new();
        };
        let max_refs = usize::from(sps.max_num_ref_frames.max(1));
        let mut mmco5 = false;
        if current.reference {
            if header.idr {
                for f in &mut self.dpb {
                    f.reference = false;
                    f.long_term = None;
                }
                if header.marking.long_term_reference {
                    current.long_term = Some(0);
                    self.max_long_term_idx = Some(0);
                } else {
                    self.max_long_term_idx = None;
                }
            } else if let Some(ops) = &header.marking.mmco {
                for op in ops {
                    match *op {
                        Mmco::ForgetShortTerm(diff) => {
                            let pic_num = i64::from(current.frame_num) - i64::from(diff) - 1;
                            let max = i64::from(sps.max_frame_num());
                            for f in self.dpb.iter_mut().filter(|f| f.short_term()) {
                                if pic_num_of(f, current.frame_num, max) == pic_num {
                                    f.reference = false;
                                }
                            }
                        }
                        Mmco::ForgetLongTerm(num) => {
                            for f in &mut self.dpb {
                                if f.long_term_ref() && f.long_term == Some(num) {
                                    f.reference = false;
                                    f.long_term = None;
                                }
                            }
                        }
                        Mmco::ShortToLong(diff, idx) => {
                            let pic_num = i64::from(current.frame_num) - i64::from(diff) - 1;
                            let max = i64::from(sps.max_frame_num());
                            let target = self.dpb.iter().position(|f| {
                                f.short_term() && pic_num_of(f, current.frame_num, max) == pic_num
                            });
                            if let Some(target) = target {
                                for (i, f) in self.dpb.iter_mut().enumerate() {
                                    if i != target && f.long_term == Some(idx) {
                                        f.reference = false;
                                        f.long_term = None;
                                    }
                                }
                                self.dpb[target].long_term = Some(idx);
                            }
                        }
                        Mmco::MaxLongTermIdx(plus1) => {
                            self.max_long_term_idx = plus1.checked_sub(1);
                            for f in &mut self.dpb {
                                if let Some(idx) = f.long_term
                                    && self.max_long_term_idx.is_none_or(|max| idx > max)
                                {
                                    f.reference = false;
                                    f.long_term = None;
                                }
                            }
                        }
                        Mmco::ForgetAll => {
                            mmco5 = true;
                            for f in &mut self.dpb {
                                f.reference = false;
                                f.long_term = None;
                            }
                            self.max_long_term_idx = None;
                        }
                        Mmco::CurrentToLong(idx) => {
                            for f in &mut self.dpb {
                                if f.long_term == Some(idx) {
                                    f.reference = false;
                                    f.long_term = None;
                                }
                            }
                            current.long_term = Some(idx);
                        }
                    }
                }
            } else {
                self.sliding_window(max_refs);
            }
            // §8.2.5.4: the buffer never holds more references than the SPS allows; a
            // stream that breaks it loses its oldest short-term ones.
            let refs = self.dpb.iter().filter(|f| f.reference).count() + 1;
            if refs > max_refs && current.long_term.is_none() {
                self.sliding_window(max_refs);
            }
        }

        if mmco5 {
            // §8.2.1: the picture's counts are taken relative to itself.
            let temp = current.poc();
            current.top_poc -= temp;
            current.bottom_poc -= temp;
            // Everything before it is output first.
            current.frame_num = 0;
        }

        // The picture order count state the next picture starts from.
        if current.reference {
            if mmco5 {
                self.poc.prev_poc_msb = 0;
                self.poc.prev_poc_lsb = current.top_poc;
            } else {
                // The MSB from the state before this picture, then the state moves on.
                let msb = self.poc_msb(&header, &sps);
                let max_lsb = i64::from(sps.max_poc_lsb());
                self.poc.prev_poc_lsb =
                    i32::try_from(i64::from(header.pic_order_cnt_lsb) % max_lsb).unwrap_or(0);
                self.poc.prev_poc_msb = msb;
            }
            self.poc.prev_ref_frame_num = current.frame_num;
        }
        self.poc.prev_frame_num_offset = if mmco5 {
            0
        } else {
            self.frame_num_offset(&header, &sps)
        };
        self.poc.prev_frame_num = current.frame_num;

        let mut outputs = Vec::new();
        if mmco5 {
            outputs.extend(self.drain());
            self.start_poc = None;
        }
        self.dpb.retain(|f| f.reference || f.waiting);
        self.dpb.push(current);

        // §C.4.5: output until the buffer fits, and as soon as more frames wait than the
        // stream reorders. A buffer full of references (a stream breaking its own level)
        // loses its oldest short-term one rather than stop.
        let capacity = usize::try_from(sps.dpb_frames()).unwrap_or(16);
        let reorder = usize::try_from(sps.reorder_frames()).unwrap_or(16);
        while self.dpb.len() > capacity {
            match self.bump() {
                Some(out) => outputs.push(out),
                None => {
                    self.sliding_window(capacity);
                    self.dpb.retain(|f| f.reference || f.waiting);
                    if self.dpb.len() > capacity {
                        break;
                    }
                }
            }
        }
        while self.dpb.iter().filter(|f| f.waiting).count() > reorder {
            match self.bump() {
                Some(out) => outputs.push(out),
                None => break,
            }
        }
        outputs
    }

    /// Outputs every frame still waiting, in order: the end of the stream.
    pub(crate) fn drain(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        while let Some(o) = self.bump() {
            out.push(o);
        }
        out
    }

    /// Outputs the waiting frame with the smallest picture order count, and drops it
    /// from the buffer when nothing references it.
    fn bump(&mut self) -> Option<Output> {
        let index = self
            .dpb
            .iter()
            .enumerate()
            .filter(|(_, f)| f.waiting)
            .min_by_key(|(_, f)| f.poc())
            .map(|(i, _)| i)?;
        let frame = &mut self.dpb[index];
        frame.waiting = false;
        let out = frame.slot.map(|slot| Output {
            slot,
            pts: frame.pts,
        });
        if !frame.reference {
            self.dpb.remove(index);
        }
        out.or_else(|| self.bump())
    }

    /// A slot no frame in the buffer holds.
    fn free_slot(&self) -> Option<usize> {
        (0..self.slots).find(|s| !self.dpb.iter().any(|f| f.slot == Some(*s)))
    }

    /// §8.2.5.3: with `max_refs` references held, the short-term one with the smallest
    /// `FrameNumWrap` stops being one.
    fn sliding_window(&mut self, max_refs: usize) {
        loop {
            let refs = self.dpb.iter().filter(|f| f.reference).count();
            if refs < max_refs {
                return;
            }
            let oldest = self
                .dpb
                .iter()
                .enumerate()
                .filter(|(_, f)| f.short_term())
                .min_by_key(|(_, f)| f.frame_num_wrap_key(self.poc.prev_ref_frame_num))
                .map(|(i, _)| i);
            let Some(oldest) = oldest else { return };
            self.dpb[oldest].reference = false;
        }
    }

    /// §8.2.5.2: frames for the `frame_num` values a stream skipped, where it may.
    fn fill_gap(&mut self, header: &SliceHeader, sps: &Sps) {
        let max = sps.max_frame_num();
        let prev = self.poc.prev_ref_frame_num;
        if header.frame_num == prev || header.frame_num == (prev + 1) % max {
            return;
        }
        if !sps.gaps_in_frame_num_allowed {
            // A lost frame: decoding goes on, and the lists use what there is.
            return;
        }
        let max_refs = usize::from(sps.max_num_ref_frames.max(1));
        let mut num = (prev + 1) % max;
        let mut guard = 0;
        while num != header.frame_num && guard < max {
            self.sliding_window(max_refs);
            self.dpb.retain(|f| f.reference || f.waiting);
            self.dpb.push(Frame {
                slot: None,
                frame_num: num,
                top_poc: 0,
                bottom_poc: 0,
                long_term: None,
                reference: true,
                waiting: false,
                pts: 0,
            });
            self.poc.prev_ref_frame_num = num;
            self.poc.prev_frame_num = num;
            num = (num + 1) % max;
            guard += 1;
        }
    }

    /// `FrameNumOffset` (§8.2.1.2, §8.2.1.3).
    fn frame_num_offset(&self, header: &SliceHeader, sps: &Sps) -> i64 {
        if header.idr {
            0
        } else if self.poc.prev_frame_num > header.frame_num {
            self.poc.prev_frame_num_offset + i64::from(sps.max_frame_num())
        } else {
            self.poc.prev_frame_num_offset
        }
    }

    /// `PicOrderCntMsb` (§8.2.1.1).
    fn poc_msb(&self, header: &SliceHeader, sps: &Sps) -> i32 {
        let (prev_msb, prev_lsb) = if header.idr {
            (0, 0)
        } else {
            (self.poc.prev_poc_msb, self.poc.prev_poc_lsb)
        };
        let max = i32::try_from(sps.max_poc_lsb()).unwrap_or(i32::MAX);
        let lsb = i32::try_from(header.pic_order_cnt_lsb).unwrap_or(0);
        if lsb < prev_lsb && prev_lsb - lsb >= max / 2 {
            prev_msb.saturating_add(max)
        } else if lsb > prev_lsb && lsb - prev_lsb > max / 2 {
            prev_msb.saturating_sub(max)
        } else {
            prev_msb
        }
    }

    /// `TopFieldOrderCnt` and `BottomFieldOrderCnt` of a frame (§8.2.1).
    fn picture_order_count(&self, header: &SliceHeader, sps: &Sps) -> (i32, i32) {
        match sps.pic_order_cnt_type {
            0 => {
                let top = self
                    .poc_msb(header, sps)
                    .saturating_add(i32::try_from(header.pic_order_cnt_lsb).unwrap_or(0));
                (top, top.saturating_add(header.delta_pic_order_cnt_bottom))
            }
            1 => {
                let offset = self.frame_num_offset(header, sps);
                let cycle = sps.offset_for_ref_frame.len() as i64;
                let mut abs = if cycle != 0 {
                    offset + i64::from(header.frame_num)
                } else {
                    0
                };
                if !header.is_reference() && abs > 0 {
                    abs -= 1;
                }
                let mut expected: i64 = 0;
                if abs > 0 {
                    let per_cycle: i64 =
                        sps.offset_for_ref_frame.iter().map(|&o| i64::from(o)).sum();
                    let cycles = (abs - 1) / cycle;
                    let in_cycle = usize::try_from((abs - 1) % cycle).unwrap_or(0);
                    expected = cycles * per_cycle
                        + sps.offset_for_ref_frame[..=in_cycle]
                            .iter()
                            .map(|&o| i64::from(o))
                            .sum::<i64>();
                }
                if !header.is_reference() {
                    expected += i64::from(sps.offset_for_non_ref_pic);
                }
                let top = expected + i64::from(header.delta_pic_order_cnt[0]);
                let bottom = top
                    + i64::from(sps.offset_for_top_to_bottom_field)
                    + i64::from(header.delta_pic_order_cnt[1]);
                (clamp_i32(top), clamp_i32(bottom))
            }
            _ => {
                let offset = self.frame_num_offset(header, sps);
                let temp = if header.idr {
                    0
                } else if header.is_reference() {
                    2 * (offset + i64::from(header.frame_num))
                } else {
                    2 * (offset + i64::from(header.frame_num)) - 1
                };
                (clamp_i32(temp), clamp_i32(temp))
            }
        }
    }

    /// The slice's two reference lists, initialised (§8.2.4.2) and modified
    /// (§8.2.4.3), each as long as the slice's active count. A gap the stream leaves
    /// is filled with the list's first frame, so the decoder never reads a missing one.
    fn reference_lists(
        &self,
        header: &SliceHeader,
        sps: &Sps,
        current: &Frame,
    ) -> (Vec<Frame>, Vec<Frame>) {
        if header.slice_type.is_intra() {
            return (Vec::new(), Vec::new());
        }
        let max = i64::from(sps.max_frame_num());
        let cur_num = current.frame_num;
        let shorts: Vec<&Frame> = self.dpb.iter().filter(|f| f.short_term()).collect();
        let mut longs: Vec<&Frame> = self.dpb.iter().filter(|f| f.long_term_ref()).collect();
        longs.sort_by_key(|f| f.long_term);

        let (init0, init1): (Vec<Frame>, Vec<Frame>) = if header.slice_type == SliceType::B {
            let cur_poc = current.poc();
            let mut before: Vec<&Frame> = shorts
                .iter()
                .copied()
                .filter(|f| f.poc() < cur_poc)
                .collect();
            before.sort_by_key(|f| std::cmp::Reverse(f.poc()));
            let mut after: Vec<&Frame> = shorts
                .iter()
                .copied()
                .filter(|f| f.poc() > cur_poc)
                .collect();
            after.sort_by_key(|f| f.poc());
            let l0: Vec<Frame> = before
                .iter()
                .chain(after.iter())
                .chain(longs.iter())
                .map(|f| **f)
                .collect();
            let mut l1: Vec<Frame> = after
                .iter()
                .chain(before.iter())
                .chain(longs.iter())
                .map(|f| **f)
                .collect();
            if l1.len() > 1 && l1 == l0 {
                l1.swap(0, 1);
            }
            (l0, l1)
        } else {
            let mut by_num = shorts.clone();
            by_num.sort_by_key(|f| std::cmp::Reverse(pic_num_of(f, cur_num, max)));
            (
                by_num.iter().chain(longs.iter()).map(|f| **f).collect(),
                Vec::new(),
            )
        };

        let n0 = usize::from(header.num_ref_idx_l0_active_minus1) + 1;
        let n1 = usize::from(header.num_ref_idx_l1_active_minus1) + 1;
        let l0 = self.modify(init0, &header.modifications_l0, n0, cur_num, max);
        let l1 = if header.slice_type == SliceType::B {
            self.modify(init1, &header.modifications_l1, n1, cur_num, max)
        } else {
            Vec::new()
        };
        (l0, l1)
    }

    /// §8.2.4.3 on one list, then sized to `active` entries.
    fn modify(
        &self,
        mut list: Vec<Frame>,
        ops: &[ListModification],
        active: usize,
        cur_num: u32,
        max: i64,
    ) -> Vec<Frame> {
        list.truncate(active);
        let mut pred = i64::from(cur_num);
        let cur = i64::from(cur_num);
        for (index, op) in ops.iter().enumerate() {
            let found = match *op {
                ListModification::ShortTermSub(diff) | ListModification::ShortTermAdd(diff) => {
                    let abs = i64::from(diff) + 1;
                    let mut no_wrap = if matches!(op, ListModification::ShortTermSub(_)) {
                        pred - abs
                    } else {
                        pred + abs
                    };
                    if no_wrap < 0 {
                        no_wrap += max;
                    } else if no_wrap >= max {
                        no_wrap -= max;
                    }
                    pred = no_wrap;
                    let pic_num = if no_wrap > cur {
                        no_wrap - max
                    } else {
                        no_wrap
                    };
                    self.dpb
                        .iter()
                        .find(|f| f.short_term() && pic_num_of(f, cur_num, max) == pic_num)
                        .copied()
                        .map(|f| (f, false))
                }
                ListModification::LongTerm(num) => self
                    .dpb
                    .iter()
                    .find(|f| f.long_term_ref() && f.long_term == Some(num))
                    .copied()
                    .map(|f| (f, true)),
            };
            let Some((frame, long)) = found else { continue };
            if index > list.len() {
                continue;
            }
            list.insert(index, frame);
            // Later copies of the same frame go.
            let mut kept = list[..=index].to_vec();
            kept.extend(list[index + 1..].iter().copied().filter(|f| {
                let same = if long {
                    f.long_term_ref() && f.long_term == frame.long_term
                } else {
                    f.short_term() && f.slot == frame.slot && f.frame_num == frame.frame_num
                };
                !same
            }));
            list = kept;
            list.truncate(active);
        }
        // Fill the gaps, and stand a real frame in for a non-existing one.
        let fill = list.iter().find(|f| f.slot.is_some()).copied().or_else(|| {
            self.dpb
                .iter()
                .find(|f| f.reference && f.slot.is_some())
                .copied()
        });
        if let Some(fill) = fill {
            for f in &mut list {
                if f.slot.is_none() {
                    *f = fill;
                }
            }
            while list.len() < active {
                list.push(fill);
            }
        }
        list
    }
}

impl Frame {
    /// The key §8.2.5.3 orders short-term frames by: `FrameNumWrap`, given the frame
    /// number of the most recent reference picture.
    fn frame_num_wrap_key(&self, latest: u32) -> i64 {
        if self.frame_num > latest {
            i64::from(self.frame_num) - (1 << 32)
        } else {
            i64::from(self.frame_num)
        }
    }
}

/// `PicNum` of a short-term frame (`FrameNumWrap`) while decoding frame `cur`.
fn pic_num_of(frame: &Frame, cur: u32, max: i64) -> i64 {
    if frame.frame_num > cur {
        i64::from(frame.frame_num) - max
    } else {
        i64::from(frame.frame_num)
    }
}

fn clamp_i32(v: i64) -> i32 {
    i32::try_from(v.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
}

#[cfg(test)]
mod tests;
