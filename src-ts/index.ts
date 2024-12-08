export type Message =
  | { type: "binary"; data: Uint8Array }
  | { type: "text"; data: string };

// @ts-expect-error
let postMessage = window.host.postMessage;

// NOTE: We cannot use lowercase `ipc`, because `wry` reserved it in the global scope.
export const IPC = {
  on: (event: "message", callback: (message: Readonly<Message>) => void) => {
    onCallbacks.push(callback);
  },
  send: (message: string | Uint8Array) => {
    if (message instanceof Uint8Array) {
      postMessage("binary," + arrayToBase64(message));
      return;
    } else if (typeof message === "string") {
      postMessage("text," + message);
      return;
    } else {
      throw new Error(
        "Invalid message type. Expected `string` or `ArrayBuffer`."
      );
    }
  },
};

const onCallbacks: ((message: Readonly<Message>) => void)[] = [];

// @ts-expect-error
window.host.onmessage = (type, data) => {
  onCallbacks.forEach((callback) => {
    const message = Object.freeze({
      type,
      data,
    });
    callback(message);
  });
};

function arrayToBase64(bytes: Uint8Array): string {
  var binary = "";
  var len = bytes.byteLength;
  for (var i = 0; i < len; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return window.btoa(binary);
}
