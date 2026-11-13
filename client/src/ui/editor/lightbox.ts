// A full-screen viewer for an image attachment (a "lightbox"), over the
// page instead of navigating to the image, so there is always a way back:
//
// - the close button (top right), Escape, or a click/tap on the dark area
//   around the image;
// - on touch screens, swiping the image down (or up) and letting go;
// - clicking/tapping the image toggles between fitting the screen and its
//   actual size (scrollable).
//
// The bar names the file and offers Download. Focus moves into the viewer
// and back to the thumbnail when it closes. Built with DOM calls only (the
// page's CSP allows no inline markup or style attributes).

export interface LightboxImage {
  /** The full image. */
  readonly src: string;
  /** Shown while the full image loads (the thumbnail, usually cached). */
  readonly placeholder?: string;
  readonly alt: string;
  readonly fileName: string;
  /** Size and the like, after the file name. */
  readonly meta?: string;
  /** Where Download points (the attachment). */
  readonly downloadHref: string;
}

const SVG = "http://www.w3.org/2000/svg";
/** How far a swipe must go to close, in CSS pixels. */
const SWIPE_CLOSE_PX = 90;

let open: { close: () => void } | null = null;

function icon(doc: Document, paths: readonly string[]): SVGSVGElement {
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", "22");
  svg.setAttribute("height", "22");
  svg.setAttribute("fill", "none");
  svg.setAttribute("stroke", "currentColor");
  svg.setAttribute("stroke-width", "2");
  svg.setAttribute("stroke-linecap", "round");
  svg.setAttribute("stroke-linejoin", "round");
  svg.setAttribute("aria-hidden", "true");
  for (const d of paths) {
    const path = doc.createElementNS(SVG, "path");
    path.setAttribute("d", d);
    svg.appendChild(path);
  }
  return svg;
}

/** Whether a lightbox is showing. */
export function lightboxOpen(): boolean {
  return open !== null;
}

/** Closes the lightbox if one is showing. */
export function closeLightbox(): void {
  open?.close();
}

/** Shows `image` over the page; returns a function that closes it. */
export function openLightbox(doc: Document, image: LightboxImage): () => void {
  open?.close();
  const view = doc.defaultView;
  const returnFocus = doc.activeElement instanceof HTMLElement ? doc.activeElement : null;

  const root = doc.createElement("div");
  root.className = "lightbox";
  root.setAttribute("role", "dialog");
  root.setAttribute("aria-modal", "true");
  root.setAttribute("aria-label", image.fileName !== "" ? image.fileName : "Image");

  const bar = doc.createElement("div");
  bar.className = "lightbox-bar";
  const titles = doc.createElement("div");
  titles.className = "lightbox-titles";
  const title = doc.createElement("span");
  title.className = "lightbox-title";
  title.textContent = image.fileName;
  titles.appendChild(title);
  if (image.meta !== undefined && image.meta !== "") {
    const meta = doc.createElement("span");
    meta.className = "lightbox-meta";
    meta.textContent = image.meta;
    titles.appendChild(meta);
  }
  const download = doc.createElement("a");
  download.className = "lightbox-btn";
  download.href = image.downloadHref;
  download.setAttribute("download", image.fileName);
  download.setAttribute("aria-label", "Download");
  download.title = "Download";
  download.appendChild(icon(doc, ["M12 4v11", "M7 10l5 5 5-5", "M5 20h14"]));
  const closeButton = doc.createElement("button");
  closeButton.type = "button";
  closeButton.className = "lightbox-btn lightbox-close";
  closeButton.setAttribute("aria-label", "Close");
  closeButton.title = "Close (Esc)";
  closeButton.appendChild(icon(doc, ["M6 6l12 12", "M18 6L6 18"]));
  bar.append(titles, download, closeButton);

  const stage = doc.createElement("div");
  stage.className = "lightbox-stage";
  const img = doc.createElement("img");
  img.className = "lightbox-img";
  img.alt = image.alt;
  img.draggable = false;
  img.src = image.placeholder ?? image.src;
  if (image.placeholder !== undefined && image.placeholder !== image.src) {
    root.classList.add("is-loading");
    const full = new (view?.Image ?? Image)();
    full.onload = () => {
      img.src = image.src;
      root.classList.remove("is-loading");
    };
    full.onerror = () => root.classList.remove("is-loading");
    full.src = image.src;
  }
  const hint = doc.createElement("span");
  hint.className = "lightbox-hint";
  hint.textContent = "Tap outside the image, press Esc or swipe down to close";
  stage.append(img);
  root.append(bar, stage, hint);

  const close = (): void => {
    if (open?.close !== close) {
      return;
    }
    open = null;
    doc.removeEventListener("keydown", onKey, true);
    doc.documentElement.classList.remove("lightbox-shown");
    root.remove();
    returnFocus?.focus({ preventScroll: true });
  };

  const onKey = (event: KeyboardEvent): void => {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      close();
    } else if (event.key === "Tab") {
      // Keep focus inside the viewer.
      const stops = [download, closeButton];
      const at = stops.indexOf(doc.activeElement as HTMLAnchorElement & HTMLButtonElement);
      event.preventDefault();
      const next = event.shiftKey ? (at <= 0 ? stops.length - 1 : at - 1) : (at + 1) % stops.length;
      stops[next]!.focus();
    }
  };

  closeButton.addEventListener("click", close);
  // A click on the dark area (not on the image or the bar) closes.
  stage.addEventListener("click", (event) => {
    if (event.target === stage && !moved) {
      close();
    }
  });
  img.addEventListener("click", (event) => {
    event.stopPropagation();
    if (!moved) {
      root.classList.toggle("is-zoomed");
    }
  });

  // Swipe to close (touch): the image follows the finger vertically and
  // the backdrop fades; far enough and it closes, else it springs back.
  let startY = 0;
  let startX = 0;
  let dragging = false;
  let moved = false;
  stage.addEventListener("pointerdown", (event) => {
    moved = false;
    if (event.pointerType === "mouse" || root.classList.contains("is-zoomed")) {
      return;
    }
    dragging = true;
    startY = event.clientY;
    startX = event.clientX;
  });
  stage.addEventListener("pointermove", (event) => {
    if (!dragging) {
      return;
    }
    const dy = event.clientY - startY;
    if (Math.abs(dy) > 8 && Math.abs(dy) > Math.abs(event.clientX - startX)) {
      moved = true;
      root.classList.add("is-dragging");
      img.style.setProperty("transform", `translateY(${dy}px)`);
      root.style.setProperty("--lightbox-fade", String(Math.max(0.35, 1 - Math.abs(dy) / 400)));
    }
  });
  const endDrag = (event: PointerEvent): void => {
    if (!dragging) {
      return;
    }
    dragging = false;
    root.classList.remove("is-dragging");
    const dy = event.clientY - startY;
    if (moved && Math.abs(dy) > SWIPE_CLOSE_PX) {
      close();
      return;
    }
    img.style.removeProperty("transform");
    root.style.removeProperty("--lightbox-fade");
  };
  stage.addEventListener("pointerup", endDrag);
  stage.addEventListener("pointercancel", endDrag);

  doc.addEventListener("keydown", onKey, true);
  doc.documentElement.classList.add("lightbox-shown");
  doc.body.appendChild(root);
  open = { close };
  closeButton.focus({ preventScroll: true });
  return close;
}
