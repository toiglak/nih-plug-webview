"use strict";

// Use Map to store callbacks with unique IDs for unsubscribing
let callbacks = new Map();
let callbackId = 0;

window.plugin = {
  listen: function (callback) {
    // Generate unique ID for this callback
    const id = ++callbackId;
    callbacks.set(id, callback);

    // Return arrow function to unsubscribe
    return () => callbacks.delete(id);
  },

  send: function (message) {
    if (typeof message !== "string") {
      throw new Error("Message must be a string");
    }
    // We attach `text,` prefix to differentiate this message from `frame` callback.
    window.plugin.__postMessage("text," + message);
  },

  // Internal method to handle incoming messages
  __onmessage: function (message) {
    // Iterate over all registered callbacks
    callbacks.forEach((callback) => callback(message));
  },

  // Internal method to post messages to the host
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
