# `nih-plug-webview`

```ts
// nih-plug-webview
declare global {
  const host: {
    onmessage: (type: "text" | "binary", message: string) => void;
    postMessage: (message: string) => void;
  };
}
```