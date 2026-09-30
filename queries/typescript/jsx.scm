; Appended to typescript/tags.scm for .tsx files: JSX component usage counts as a call.

(jsx_opening_element
  name: (identifier) @name) @reference.call

(jsx_self_closing_element
  name: (identifier) @name) @reference.call

(jsx_closing_element
  name: (identifier) @name) @reference.call

; `onClick={handler}`
(jsx_expression
  (identifier) @name) @reference.value

; property reads inside JSX (`{store.count}`), see tags.scm
(jsx_expression
  (member_expression
    object: (_) @qualifier
    property: [(property_identifier) (private_property_identifier)] @name) @reference.value)
