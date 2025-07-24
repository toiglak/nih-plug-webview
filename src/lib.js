window.__NIH_PLUG_WEBVIEW__ = {
  onmessage: function (type, data) {},
  postMessage: function (message) {
    if (typeof message !== "string") {
      throw new Error("Message must be a string");
    }
    window.ipc.postMessage(message);
  },
  decodeBase64: function (base64) {
    var binaryString = atob(base64);
    var bytes = new Uint8Array(binaryString.length);
    for (var i = 0; i < binaryString.length; i++) {
      bytes[i] = binaryString.charCodeAt(i);
    }
    return bytes.buffer;
  },
};

// Every frame, send postMessage to the main process (simulate `on_frame`, todo:
// completely remove it).
function loop() {
  requestAnimationFrame(loop);
  window.__NIH_PLUG_WEBVIEW__.postMessage("frame");
}

loop();
