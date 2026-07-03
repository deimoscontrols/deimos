function activeThemeVariant() {
  return document.body.getAttribute("data-md-color-scheme") === "default"
    ? "light"
    : "dark";
}

function updateThemeAssets() {
  const variant = activeThemeVariant();

  document.querySelectorAll("[data-theme-src-dark][data-theme-src-light]").forEach((element) => {
    const nextSource = element.dataset[`themeSrc${variant[0].toUpperCase()}${variant.slice(1)}`];

    if (!nextSource || element.getAttribute("src") === nextSource) {
      return;
    }

    const frame = element.closest(".bode-plot-frame");
    frame?.classList.remove("bode-plot-frame--loaded");
    element.setAttribute("src", nextSource);
  });
}

function initThemeAssets() {
  updateThemeAssets();

  const observer = new MutationObserver(updateThemeAssets);
  observer.observe(document.body, {
    attributes: true,
    attributeFilter: ["data-md-color-scheme"],
  });
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initThemeAssets);
} else {
  initThemeAssets();
}
