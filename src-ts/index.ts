export type Message =
  | { type: "binary"; data: ArrayBuffer }
  | { type: "text"; data: string };

const onCallbacks = [];

// NOTE: We cannot use lowercase `ipc`, because `wry` reserved it in the global scope.
export const IPC = {
  on: (event: "message", callback: (message: Message) => void) => {
    onCallbacks.push(callback);
  },
  send: (message: string | ArrayBuffer) => {
    if (message instanceof ArrayBuffer) {
      // @ts-ignore
      window.host.postMessage("binary," + arrayBufferToBase64(message));
      return;
    } else if (typeof message === "string") {
      // @ts-ignore
      window.host.postMessage("text," + message);
      return;
    } else {
      throw new Error(
        "Invalid message type. Expected `string` or `ArrayBuffer`."
      );
    }
  },
};

// @ts-ignore
window.host.onmessage = (type, data) => {
  onCallbacks.forEach((callback) => callback({ type, data }));
};

function arrayBufferToBase64(bytes) {
  var binary = "";
  var len = bytes.byteLength;
  for (var i = 0; i < len; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return window.btoa(binary);
}
