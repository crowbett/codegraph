; codegraph tags for JavaScript / JSX (see go/tags.scm for capture conventions).
; Extra: @export — an export statement (default / named export bookkeeping).

; ---- definitions ----------------------------------------------------------
(function_declaration
  name: (identifier) @name) @definition.function

(generator_function_declaration
  name: (identifier) @name) @definition.function

(class_declaration
  name: (identifier) @name) @definition.class

(method_definition
  name: [(property_identifier) (private_property_identifier)] @name) @definition.method

(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

(variable_declaration
  (variable_declarator
    name: (identifier) @name
    value: [(arrow_function) (function_expression) (generator_function)]) @definition.function)

(lexical_declaration
  (variable_declarator
    name: (identifier) @name
    value: (class)) @definition.class)

(program
  (lexical_declaration
    (variable_declarator
      name: (identifier) @name
      value: [(string) (number) (template_string) (object) (array)]) @definition.const))

(program
  (export_statement
    (lexical_declaration
      (variable_declarator
        name: (identifier) @name
        value: [(string) (number) (template_string) (object) (array)]) @definition.const)))

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (member_expression
    object: (_) @qualifier
    property: [(property_identifier) (private_property_identifier)] @name)) @reference.call

(new_expression
  constructor: (identifier) @name) @reference.call

(new_expression
  constructor: (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name)) @reference.call

(class_heritage
  (identifier) @name) @reference.inherit

(class_heritage
  (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name)) @reference.inherit

(jsx_opening_element
  name: (identifier) @name) @reference.call

(jsx_self_closing_element
  name: (identifier) @name) @reference.call

; callbacks passed as values
(arguments
  (member_expression
    object: (_) @qualifier
    property: (property_identifier) @name) @reference.value)

(arguments
  (identifier) @name @reference.value)

; ---- imports / exports ----------------------------------------------------
(import_statement) @import

((variable_declarator
  name: (_)
  value: (call_expression
    function: (identifier) @_fn
    arguments: (arguments (string)))) @import
  (#eq? @_fn "require"))

(export_statement) @export

; re-exports: `export * from './x'`, `export { a as b } from './x'`
(export_statement
  source: (string)) @import
