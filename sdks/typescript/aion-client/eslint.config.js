import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";

export default [
  js.configs.recommended,
  {
    ignores: ["dist/**"],
  },
  {
    files: ["src/**/*.ts"],
    languageOptions: {
      ecmaVersion: "latest",
      parser: tsParser,
      sourceType: "module",
    },
    rules: {
      "no-empty": "error",
      "no-throw-literal": "error",
      "no-undef": "off",
      "no-unused-vars": "off",
    },
  },
];
