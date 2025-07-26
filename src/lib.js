"use strict";

let callbacks = [];

window.plugin = {
  listen: function (callback) {
    callbacks.push(callback);
  },
  send: function (message) {
    if (typeof message !== "string") {
      throw new Error("Message must be a string");
    }
    // We attach `text,` prefix to differentiate this message from `frame` callback.
    window.ipc.postMessage("text," + message);
  },

  __onmessage: function (message) {
    callbacks.forEach((callback) => callback(message));
  },
  __postMessage: function (message) {
    window.ipc.postMessage(message);
  },
};

// Every frame, send postMessage to the main process to call `on_frame`.
// TODO: Figure out how to remove this (or if we really want to remove this).
function loop() {
  requestAnimationFrame(loop);
  window.plugin.__postMessage("frame");
}

loop();
