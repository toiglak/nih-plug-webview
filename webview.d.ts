interface Plugin {
  listen: (callback: (message: string) => void) => void;
  send: (message: string) => void;
}

declare global {
  interface Window {
    plugin: Plugin;
  }
}

export {};
