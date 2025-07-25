import { IPC } from ".."; // "nih-plug-webview"

const button = document.querySelector("button")!;
const response = document.getElementById("response")!;

IPC.on("message", (message) => {
  response.textContent = message;
});

button.addEventListener("click", () => {
  IPC.send("Hello, world!");
});
