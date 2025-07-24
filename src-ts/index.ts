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
  on: (event: "message", callback: (message: string) => void) => {
    onCallbacks.push(callback);
  },

  /**
   * Sends a message to the plugin. The message can be either a string or a Uint8Array.
   *
   * @throws Will throw an error if the message type is not a string or Uint8Array.
   */
  send: (message: string) => {
      plugin.postMessage(message);
  },
};

///////////////////////////////////////////////////////////////////////////////

// @ts-expect-error
const plugin = window.__NIH_PLUG_WEBVIEW__;

const onCallbacks: ((message: string) => void)[] = [];

plugin.onmessage = (message: string) => {
  onCallbacks.forEach((callback) => {
    callback(message);
  });
};