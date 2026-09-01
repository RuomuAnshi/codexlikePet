const prop = document.querySelector<HTMLElement>("#prop")!;
const kind = new URLSearchParams(globalThis.location.search).get("kind") ?? "toy";
prop.dataset.kind = kind;
