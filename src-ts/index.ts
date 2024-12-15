export type Message =
  | { type: "binary"; data: Uint8Array }
  | { type: "text"; data: string };

/**
 * IPC (Inter-Process Communication) object used to send and receive messages from the
 * plugin.
 */
// NOTE: We cannot use lowercase `ipc`, because `wry` reserved it in the global scope.
export const IPC = {
  /**
   * Appends an event listener for the specified event.
   * - `message` event is emitted when a message is received from the plugin.
   */
  on: (event: "message", callback: (message: Message) => void) => {
    onCallbacks.push(callback);
  },

  /**
   * Sends a message to the plugin. The message can be either a string or a Uint8Array.
   *
   * @throws Will throw an error if the message type is not a string or Uint8Array.
   */
  send: (message: string | Uint8Array) => {
    if (message instanceof Uint8Array) {
      plugin.postMessage("binary," + arrayToBase64(message));
      return;
    } else if (typeof message === "string") {
      plugin.postMessage("text," + message);
      return;
    } else {
      throw new Error(
        "Invalid message type. Expected `string` or `ArrayBuffer`."
      );
    }
  },
};

///////////////////////////////////////////////////////////////////////////////

// @ts-expect-error
const plugin = window.__NIH_PLUG_WEBVIEW__;

const onCallbacks: ((message: Message) => void)[] = [];

plugin.onmessage = (type: any, data: any) => {
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
