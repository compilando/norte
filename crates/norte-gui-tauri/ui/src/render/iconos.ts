// Los iconos de la barra de actividad (spec 2026-09-21): uno por panel de
// serie, dibujados aquí con trazos simples sobre una rejilla de 24.
//
// Son PROPIOS y no de un juego de iconos ajeno: seis trazos no valen una
// dependencia ni una licencia más que auditar. Pintan con `currentColor`, así
// que el estado del botón —cerrado, abierto, con el teclado— y el tema los
// colorean sin que esta tabla sepa nada de ellos.
//
// Un kind que no está aquí —el de un plugin, o uno nuevo que se olvidó— no se
// queda sin botón: el renderer pinta su LETRA, que es lo que ya se sabe de él.

const SVG = "http://www.w3.org/2000/svg";

/** Las piezas de un icono: trazos `d` y círculos `[cx, cy, r]`. */
interface Figura {
  paths: string[];
  circles?: [number, number, number][];
}

const FIGURAS: Record<string, Figura> = {
  // Sitios: una estrella, lo marcado como favorito.
  places: {
    paths: ["M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"],
  },
  // Árbol: una carpeta arriba y dos ramas que cuelgan de ella.
  tree: {
    paths: ["M4 4h6v5H4z", "M14 11h6v4h-6z", "M14 17h6v4h-6z", "M7 9v10h7", "M7 13h7"],
  },
  // Visor: un ojo.
  viewer: {
    paths: ["M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"],
    circles: [[12, 12, 3]],
  },
  // Procesos: el pulso de algo que está trabajando.
  processes: { paths: ["M3 12h4l2.5-6 5 12 2.5-6h4"] },
  // Detalles: la «i» de información.
  metadata: { paths: ["M12 11v6", "M12 7.5v.5"], circles: [[12, 12, 9]] },
  // Registro: líneas de texto que se acumulan.
  log: { paths: ["M5 6h14", "M5 10h14", "M5 14h10", "M5 18h7"] },
  // Mapa de disco: un queso con una porción fuera.
  "disk-map": { paths: ["M11 4a8 8 0 1 0 9 9h-9z", "M14 3.5a7 7 0 0 1 6.5 6.5H14z"] },
  // Línea de tiempo: un reloj.
  timeline: { paths: ["M12 7v5l3.5 2"], circles: [[12, 12, 9]] },
  // Los botones de disposición (ADR 0133), con el prefijo `layout:` para
  // que no se crucen con un kind de panel.
  // Partir lado a lado: un marco con una raya vertical.
  "layout:split-h": { paths: ["M3.5 5h17v14h-17z", "M12 5v14"] },
  // Partir arriba y abajo: la raya en horizontal.
  "layout:split-v": { paths: ["M3.5 5h17v14h-17z", "M3.5 12h17"] },
  // Igualar: la raya en medio y dos mitades iguales a cada lado.
  "layout:equalize": {
    paths: ["M3.5 5h17v14h-17z", "M12 5v14", "M6.5 12h3", "M14.5 12h3"],
  },
  // Elegir disposición: una rejilla de cuatro.
  "layout:pick": {
    paths: ["M4 4h7v7H4z", "M13 4h7v7h-7z", "M4 13h7v7H4z", "M13 13h7v7h-7z"],
  },
  // Historia de git: una rama que se separa y vuelve.
  gitlog: {
    paths: ["M6 8v8", "M18 8.5c0 5-6 4-11 7"],
    circles: [
      [6, 5.5, 2.5],
      [6, 18.5, 2.5],
      [18, 6, 2.5],
    ],
  },
  // Lo que hay en el árbol y en los sitios (prefijo `fs:`).
  // Carpeta cerrada: la pestaña arriba a la izquierda.
  "fs:folder": { paths: ["M3.5 6.5h6l2 2h9v10h-17z"] },
  // Carpeta abierta: la tapa inclinada hacia delante.
  "fs:folder-open": { paths: ["M3.5 18.5v-12h6l2 2h8v2", "M3.5 18.5l3-8h15l-3 8z"] },
  // Un disco: la caja con su piloto.
  "fs:drive": { paths: ["M3.5 7.5h17v9h-17z", "M3.5 13h17"], circles: [[17, 15, 0.6]] },
  // Un disco extraíble: el conector.
  "fs:removable": { paths: ["M8 3.5h8v5H8z", "M6 8.5h12v12H6z"] },
  // Algo en red: el globo.
  "fs:network": {
    paths: ["M3 12h18", "M12 3c3 3 3 15 0 18", "M12 3c-3 3-3 15 0 18"],
    circles: [[12, 12, 9]],
  },
  // La casa.
  "fs:home": { paths: ["M4 11l8-7 8 7", "M6 9.5v10h12v-10"] },
  // Un favorito: la estrella de «sitios».
  "fs:favorite": {
    paths: ["M12 3.5l2.6 5.3 5.9.9-4.3 4.1 1 5.8L12 16.9l-5.2 2.7 1-5.8-4.3-4.1 5.9-.9z"],
  },
};

/**
 * El icono de `id` —un kind de panel, un botón `layout:*` o una cosa del
 * sistema de ficheros `fs:*`—, o `null` si no tiene uno propio.
 *
 * Decorativo para la accesibilidad (`aria-hidden`): el nombre va al lado o
 * en la etiqueta, y un lector que leyera también el dibujo diría dos veces lo
 * mismo.
 */
export function icono(doc: Document, kind: string): SVGSVGElement | null {
  const figura = FIGURAS[kind];
  if (figura === undefined) {
    return null;
  }
  const svg = doc.createElementNS(SVG, "svg");
  svg.setAttribute("class", "panelbar-icon");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  svg.setAttribute("focusable", "false");
  for (const d of figura.paths) {
    const p = doc.createElementNS(SVG, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  for (const [cx, cy, r] of figura.circles ?? []) {
    const c = doc.createElementNS(SVG, "circle");
    c.setAttribute("cx", String(cx));
    c.setAttribute("cy", String(cy));
    c.setAttribute("r", String(r));
    svg.append(c);
  }
  return svg;
}

/** La cifra de una insignia: a partir de cien, `99+`. */
export function cifraDeInsignia(n: number): string {
  return n > 99 ? "99+" : String(n);
}
