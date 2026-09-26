; Appended to typescript/tags.scm for .tsx files: JSX component usage counts as a call.

(jsx_opening_element
  name: (identifier) @name) @reference.call

(jsx_self_closing_element
  name: (identifier) @name) @reference.call
