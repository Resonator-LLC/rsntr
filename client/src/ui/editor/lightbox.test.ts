// @vitest-environment happy-dom
import { afterEach, describe, expect, it } from "vitest";
import { closeLightbox, lightboxOpen, openLightbox } from "./lightbox";

const image = {
  src: "/attachment/a.example/x",
  placeholder: "/thumbnail/a.example/x",
  alt: "plan",
  fileName: "plan.png",
  meta: "12 KB",
  downloadHref: "/attachment/a.example/x",
};

const box = (): HTMLElement | null => document.querySelector(".lightbox");

describe("the image viewer", () => {
  afterEach(() => closeLightbox());

  it("shows the image with a title, Download and Close, and takes the focus", () => {
    const thumb = document.createElement("a");
    thumb.href = "#";
    document.body.appendChild(thumb);
    thumb.focus();
    openLightbox(document, image);
    expect(lightboxOpen()).toBe(true);
    expect(box()?.getAttribute("role")).toBe("dialog");
    expect(box()?.querySelector(".lightbox-title")?.textContent).toBe("plan.png");
    expect(box()?.querySelector("a[download]")?.getAttribute("href")).toBe(image.downloadHref);
    expect(document.activeElement?.classList.contains("lightbox-close")).toBe(true);
    expect(document.documentElement.classList.contains("lightbox-shown")).toBe(true);
    (box()?.querySelector(".lightbox-close") as HTMLButtonElement).click();
    expect(box()).toBe(null);
    expect(document.documentElement.classList.contains("lightbox-shown")).toBe(false);
    // Back to where it was opened from.
    expect(document.activeElement).toBe(thumb);
  });

  it("closes on Escape and on a click outside the image, not on the image", () => {
    openLightbox(document, image);
    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(box()).toBe(null);

    openLightbox(document, image);
    const img = box()!.querySelector(".lightbox-img") as HTMLImageElement;
    img.click();
    expect(box()).not.toBe(null);
    expect(box()!.classList.contains("is-zoomed")).toBe(true);
    (box()!.querySelector(".lightbox-stage") as HTMLElement).click();
    expect(box()).toBe(null);
  });

  it("shows one at a time", () => {
    openLightbox(document, image);
    openLightbox(document, { ...image, fileName: "other.png" });
    expect(document.querySelectorAll(".lightbox").length).toBe(1);
    expect(box()?.querySelector(".lightbox-title")?.textContent).toBe("other.png");
  });
});
