import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  js.configs.recommended,
  ...tseslint.configs.recommendedTypeChecked,
  {
    languageOptions: {
      parserOptions: { projectService: true, tsconfigRootDir: import.meta.dirname },
    },
    rules: {
      // `const { update: _u, ...resto }` es la forma de QUITAR la etiqueta de
      // un enum etiquetado; el descarte se marca con `_`.
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // La frontera con la webview: nada de HTML crudo, nunca (decisión D11).
      // Un nombre de fichero, una cadena de plugin o una ayuda son DATOS.
      "no-restricted-properties": [
        "error",
        { object: "document", property: "write" },
        {
          property: "innerHTML",
          message: "prohibido: el texto se pone con textContent (decisión D11)",
        },
        {
          property: "outerHTML",
          message: "prohibido: el texto se pone con textContent (decisión D11)",
        },
        // Los otros cuatro sumideros de HTML. La regla existía para dos, y
        // el código estaba limpio — lo que tenía agujeros era la regla que
        // debía mantenerlo limpio.
        {
          property: "insertAdjacentHTML",
          message: "prohibido: el texto se pone con textContent (decisión D11)",
        },
        {
          property: "setHTMLUnsafe",
          message: "prohibido: el texto se pone con textContent (decisión D11)",
        },
        {
          property: "srcdoc",
          message: "prohibido: la webview no anida documentos (decisión D11)",
        },
        {
          property: "createContextualFragment",
          message: "prohibido: parsea HTML de una cadena (decisión D11)",
        },
      ],
      "no-restricted-globals": [
        "error",
        { name: "eval", message: "la CSP lo prohíbe y el código tampoco lo necesita" },
      ],
      // Un `style="..."` puesto por atributo lo bloquea la CSP; el estilo
      // dinámico se pone por CSSOM (`el.style.setProperty`).
      "no-restricted-syntax": [
        "error",
        {
          selector:
            "CallExpression[callee.property.name='parseFromString']",
          message: "prohibido: parsea HTML de una cadena (decisión D11)",
        },
        {
          selector: "NewExpression[callee.name='DOMParser']",
          message: "prohibido: la webview no parsea HTML (decisión D11)",
        },
        {
          selector:
            "CallExpression[callee.property.name='setAttribute'][arguments.0.value='style']",
          message: "la CSP bloquea el atributo style; usa el.style.setProperty",
        },
      ],
    },
  },
  { ignores: ["dist/**", "coverage/**"] },
);
