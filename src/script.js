window.plugin = {};

window.plugin.send_message = (message) => {
  window.ipc.postMessage && window.ipc.postMessage(JSON.stringify(message));
};

window.plugin.on_message_internal = (message) => {
  window.plugin.on_message && window.plugin.on_message(JSON.parse(message));
};
