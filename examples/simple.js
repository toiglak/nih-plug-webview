const button = document.querySelector("button");
const response = document.getElementById("response");

window.plugin.listen((message) => {
  response.textContent = message;
});

button.addEventListener("click", () => {
  window.plugin.send("Hello from JS!");
});
