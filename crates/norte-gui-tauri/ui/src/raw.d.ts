// Vite's `?raw` import: the file's text, bundled at build time.
declare module "*?raw" {
  const text: string;
  export default text;
}
