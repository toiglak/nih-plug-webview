window.__NIH_PLUG_WEBVIEW__ = {
  onmessage: function (message) { },
  postMessage: function (message) {
    // todo: this can probably be removed 
    if (typeof message !== "string") {
      throw new Error("Message must be a string");
    }
    window.ipc.postMessage(message);
  },
};

// Every frame, send postMessage to the main process (simulate `on_frame`, todo:
// completely remove it).
function loop() {
  requestAnimationFrame(loop);
  window.__NIH_PLUG_WEBVIEW__.postMessage("frame");
}

loop();
