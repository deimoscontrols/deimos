document.addEventListener("DOMContentLoaded", () => {
  const dialog = document.createElement("dialog");
  dialog.className = "image-zoom-dialog";
  dialog.innerHTML = `
    <button class="image-zoom-dialog__close" type="button" aria-label="Close image">&times;</button>
    <img alt="">
  `;
  document.body.appendChild(dialog);

  const dialogImage = dialog.querySelector("img");
  const closeButton = dialog.querySelector(".image-zoom-dialog__close");

  function openImage(image) {
    dialogImage.src = image.currentSrc || image.src;
    dialogImage.alt = image.alt || "";
    dialog.showModal();
    closeButton.focus();
  }

  function zoomableImageFromEvent(event) {
    if (!(event.target instanceof Element)) {
      return null;
    }
    return event.target.closest(".zoomable-image");
  }

  document.querySelectorAll(".zoomable-image").forEach((image) => {
    image.tabIndex = 0;
    image.setAttribute("role", "button");
    image.setAttribute("aria-label", image.alt ? `Open larger image: ${image.alt}` : "Open larger image");
  });

  document.addEventListener("click", (event) => {
    const image = zoomableImageFromEvent(event);
    if (!image) {
      return;
    }

    event.preventDefault();
    openImage(image);
  });

  document.addEventListener("keydown", (event) => {
    const image = zoomableImageFromEvent(event);
    if (!image || !["Enter", " "].includes(event.key)) {
      return;
    }

    event.preventDefault();
    openImage(image);
  });

  closeButton.addEventListener("click", () => {
    dialog.close();
  });

  dialog.addEventListener("click", (event) => {
    if (event.target === dialog) {
      dialog.close();
    }
  });
});
