import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  // Of the crates, only the runtime's TypeScript Node modules are linted.
  { ignores: ["**/node_modules/**", "target/**", "crates/*", "!crates/ply-jsc/", "crates/ply-jsc/*", "!crates/ply-jsc/src/", "crates/ply-jsc/src/*", "!crates/ply-jsc/src/node/"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.ts"],
    rules: {
      "@typescript-eslint/consistent-type-imports": "error",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_" }],
      "max-lines": ["error", { max: 250, skipBlankLines: true, skipComments: true }]
    }
  },
  {
    files: ["packages/sdk/**/*.d.ts"],
    // Inline type imports keep ambient module declarations in script scope.
    rules: { "@typescript-eslint/consistent-type-imports": ["error", { disallowTypeAnnotations: false }] }
  },
  {
    files: ["plugins/tui/text.ts", "plugins/tui/composer.ts", "plugins/tui/keys.ts"],
    // Terminal protocol parsing and sanitization deliberately match controls.
    rules: { "no-control-regex": "off" }
  }
);
