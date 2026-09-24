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
      // `const { update: _u, ...rest }` is how you REMOVE the tag from a
      // tagged enum; the discard is marked with `_`.
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // The boundary with the webview: no raw HTML, ever (decision D11). A
      // file name, a plugin string or a help entry are DATA.
      "no-restricted-properties": [
        "error",
        { object: "document", property: "write" },
        {
          property: "innerHTML",
          message: "forbidden: text goes through textContent (decision D11)",
        },
        {
          property: "outerHTML",
          message: "forbidden: text goes through textContent (decision D11)",
        },
        // The other four HTML sinks. The rule existed for two, and the code
        // was clean — what had holes was the rule meant to keep it clean.
        {
          property: "insertAdjacentHTML",
          message: "forbidden: text goes through textContent (decision D11)",
        },
        {
          property: "setHTMLUnsafe",
          message: "forbidden: text goes through textContent (decision D11)",
        },
        {
          property: "srcdoc",
          message: "forbidden: the webview doesn't nest documents (decision D11)",
        },
        {
          property: "createContextualFragment",
          message: "forbidden: parses HTML from a string (decision D11)",
        },
      ],
      "no-restricted-globals": [
        "error",
        { name: "eval", message: "the CSP forbids it and the code doesn't need it either" },
      ],
      // A `style="..."` set by attribute is blocked by the CSP; dynamic
      // style is set through CSSOM (`el.style.setProperty`).
      "no-restricted-syntax": [
        "error",
        {
          selector:
            "CallExpression[callee.property.name='parseFromString']",
          message: "forbidden: parses HTML from a string (decision D11)",
        },
        {
          selector: "NewExpression[callee.name='DOMParser']",
          message: "forbidden: the webview doesn't parse HTML (decision D11)",
        },
        {
          selector:
            "CallExpression[callee.property.name='setAttribute'][arguments.0.value='style']",
          message: "the CSP blocks the style attribute; use el.style.setProperty",
        },
      ],
    },
  },
  { ignores: ["dist/**", "coverage/**"] },
);
