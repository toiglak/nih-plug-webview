window.__NIH_PLUG_WEBVIEW__ = {
  onmessage: function (message) {},
  postMessage: function (message) {
    if (typeof message !== "string") {
      throw new Error("Message must be a string");
    }
    window.ipc.postMessage(message);
  },
};

// Every frame, send postMessage to the main process to call `on_frame`.
// TODO: Figure out how to remove this (or if we really want to remove this).
function loop() {
  requestAnimationFrame(loop);
  window.__NIH_PLUG_WEBVIEW__.postMessage("frame");
}

loop();
