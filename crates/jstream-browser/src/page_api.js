// The page API (ADR-0102): `window.jstream`, defined in a browser source's main frame
// before any page script runs. The helper evaluates this with the grant's members passed
// in, and keeps the function it returns: the only way data reaches the API.
// Nothing here sends anything anywhere.
(function (grant) {
  "use strict";
  var listeners = Object.create(null);
  var anchor = null;
  var source = { shown: false, previewShown: false, shownAt: null, previewShownAt: null };
  var scenes = { program: null, preview: null };
  var values = new Map();

  function emit(type, detail) {
    var set = listeners[type];
    if (!set) {
      return;
    }
    Array.from(set).forEach(function (listener) {
      try {
        listener(detail);
      } catch (error) {
        // Reported as an uncaught error, after the update is done.
        setTimeout(function () {
          throw error;
        });
      }
    });
  }

  function scene(s) {
    return s === null ? null : Object.freeze({ id: s.id, name: s.name });
  }

  var api = {
    version: 1,
    clock: Object.freeze({
      now: function () {
        if (anchor === null) {
          return 0;
        }
        return anchor.t + anchor.lead + (performance.now() - anchor.at) / 1000;
      },
    }),
    source: Object.freeze({
      get shown() {
        return source.shown;
      },
      get previewShown() {
        return source.previewShown;
      },
      get shownAt() {
        return source.shownAt;
      },
      get previewShownAt() {
        return source.previewShownAt;
      },
    }),
    addEventListener: function (type, listener) {
      if (typeof listener !== "function") {
        return;
      }
      (listeners[String(type)] = listeners[String(type)] || new Set()).add(listener);
    },
    removeEventListener: function (type, listener) {
      var set = listeners[String(type)];
      if (set) {
        set.delete(listener);
      }
    },
  };
  if (grant.scenes) {
    Object.defineProperty(api, "scenes", {
      enumerable: true,
      get: function () {
        return Object.freeze({ program: scene(scenes.program), preview: scene(scenes.preview) });
      },
    });
  }
  if (grant.values) {
    api.values = Object.freeze({
      // A fresh copy each time, parsed from the JSON text the engine sent.
      get: function (name) {
        var text = values.get(String(name));
        return text === undefined ? undefined : JSON.parse(text);
      },
    });
  }
  Object.defineProperty(window, "jstream", {
    value: Object.freeze(api),
    enumerable: true,
    configurable: false,
    writable: false,
  });

  // One update: JSON text with any of `clock`, `source`, `scenes` and `values`.
  return function (text) {
    var update = JSON.parse(text);
    if (update.clock) {
      anchor = { t: update.clock.t, lead: update.clock.lead, at: performance.now() };
    }
    if (update.source) {
      var was = source;
      source = update.source;
      if (source.shown !== was.shown) {
        emit(source.shown ? "shown" : "hidden", { on: "program", at: source.shownAt });
      }
      if (source.previewShown !== was.previewShown) {
        emit(source.previewShown ? "shown" : "hidden", { on: "preview", at: source.previewShownAt });
      }
    }
    // A document is sent the whole state again when it starts loading, so only what
    // differs from what the page holds is reported.
    if (grant.scenes && update.scenes && JSON.stringify(update.scenes) !== JSON.stringify(scenes)) {
      scenes = update.scenes;
      emit("scenes", api.scenes);
    }
    if (grant.values && update.values) {
      Object.keys(update.values).forEach(function (name) {
        var value = update.values[name];
        if (value === null) {
          if (values.delete(name)) {
            emit("value", { name: name, value: null });
          }
        } else if (values.get(name) !== JSON.stringify(value)) {
          values.set(name, JSON.stringify(value));
          emit("value", { name: name, value: JSON.parse(values.get(name)) });
        }
      });
    }
  };
})(__GRANT__)
