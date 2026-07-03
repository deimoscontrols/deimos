function activeThemeVariant() {
  return document.body.getAttribute("data-md-color-scheme") === "default"
    ? "light"
    : "dark";
}

function updateThemeAssets() {
  const variant = activeThemeVariant();
  const sourceKey = variant === "light" ? "themeSrcLight" : "themeSrcDark";

  document.querySelectorAll("[data-theme-src-dark][data-theme-src-light]").forEach((element) => {
    const nextSource = element.dataset[sourceKey];

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

  if (window.document$ && typeof window.document$.subscribe === "function") {
    window.document$.subscribe(updateThemeAssets);
  }

  window.addEventListener("load", updateThemeAssets);
  setTimeout(updateThemeAssets, 0);
  setTimeout(updateThemeAssets, 250);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", initThemeAssets);
} else {
  initThemeAssets();
}
