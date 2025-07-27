const button = document.querySelector("button");
const response = document.getElementById("response");

window.plugin.listen((message) => {
  // Listen for messages from Rust and update DOM
  response.textContent = message;
});

button.addEventListener("click", () => {
  // Send a message to Rust when the button is clicked
  window.plugin.send("Hello from JS!");
});
