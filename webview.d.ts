interface Plugin {
  /**
   * Registers a callback to receive messages from the plugin host.
   * @param callback - Function invoked when a message is received.
   * @returns A function to unsubscribe and remove the listener.
   */
  listen: (callback: (message: string) => void) => () => void;

  /**
   * Send a message to the plugin host
   * @param {string} message - Message to send (must be a string)
   */
  send: (message: string) => void;

  /**
   * Request the plugin host to resize the window
   * @param {number} width - New width in logical pixels
   * @param {number} height - New height in logical pixels
   */
  resize: (width: number, height: number) => void;
}

declare global {
  interface Window {
    plugin: Plugin;
  }
}

export {};
