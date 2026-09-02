const prop = document.querySelector<HTMLElement>("#prop")!;
const requestedKind = new URLSearchParams(globalThis.location.search).get("kind") ?? "plush";
const kind = ["football", "snack", "plush", "ribbon"].includes(requestedKind)
  ? requestedKind
  : "plush";
prop.dataset.kind = kind;

const image = document.createElement("img");
image.src = `/props/${kind}/sprite.svg`;
image.alt = "";
image.draggable = false;
prop.replaceChildren(image);
