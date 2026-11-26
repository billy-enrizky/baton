(() => {

  const status = (el, text) => {
    const target = el.parentElement.querySelector(".copy-status");
    if (!target) return;
    target.textContent = text;
    window.setTimeout(() => { target.textContent = ""; }, 1800);
  };

  for (const button of document.querySelectorAll("[data-copy]")) {
    button.addEventListener("click", async () => {
      const source = document.getElementById(button.dataset.copy);
      if (!source) return;
      try {
        await navigator.clipboard.writeText(source.textContent.trim());
        status(button, "Copied");
      } catch {
        status(button, "Select the text to copy");
      }
    });
  }

  const tabs = [...document.querySelectorAll('[role="tab"]')];
  const panels = [...document.querySelectorAll('[role="tabpanel"]')];
  const select = (tab) => {
    for (const item of tabs) {
      const active = item === tab;
      item.classList.toggle("is-active", active);
      item.setAttribute("aria-selected", active ? "true" : "false");
    }
    for (const panel of panels) {
      const active = panel.id === tab.getAttribute("aria-controls");
      panel.classList.toggle("is-active", active);
      panel.hidden = !active;
    }
  };
  for (const tab of tabs) {
    tab.addEventListener("click", () => select(tab));
    tab.addEventListener("keydown", (event) => {
      const index = tabs.indexOf(tab);
      if (event.key === "ArrowRight") select(tabs[(index + 1) % tabs.length]);
      if (event.key === "ArrowLeft") select(tabs[(index + tabs.length - 1) % tabs.length]);
    });
  }

})();
