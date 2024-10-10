# `nih-plug-webview`

```ts
// nih-plug-webview
declare global {
  const host: {
    onmessage: (message: string) => void;
    postMessage: (message: string) => void;
  };
}
```

## Example usage

```ts
host.onmessage = (message: string) => {
  let data = JSON.parse(message);
  host.postMessage("Pong");
};
```
