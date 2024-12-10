window.__NIH_PLUG_WEBVIEW__ = {
  messageBuffer: [],
  onmessage: function (type, data) {
    window.__NIH_PLUG_WEBVIEW__.messageBuffer.push({ type, data });
  },
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
