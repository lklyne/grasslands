const status = document.getElementById("script-status");
status.textContent = "script.js ran";
status.className = "good";

const counter = document.getElementById("counter");
counter.addEventListener("click", () => {
  counter.textContent = String(Number(counter.textContent) + 1);
});
