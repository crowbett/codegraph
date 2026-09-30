; codegraph tags for Go. Capture conventions follow tree-sitter tags.scm:
;   @name + @definition.<kind> | @reference.<kind>; extras: @qualifier, @receiver,
;   @import (whole import declaration), @package (package clause).

(package_clause (package_identifier) @name) @package

; ---- definitions ----------------------------------------------------------
(function_declaration
  name: (identifier) @name) @definition.function

(method_declaration
  receiver: (parameter_list) @receiver
  name: (field_identifier) @name) @definition.method

(type_spec
  name: (type_identifier) @name
  type: (struct_type)) @definition.struct

(type_spec
  name: (type_identifier) @name
  type: (interface_type)) @definition.interface

(type_spec
  name: (type_identifier) @name
  type: [(type_identifier) (qualified_type) (pointer_type) (map_type) (slice_type)
         (function_type) (array_type) (channel_type) (generic_type)]) @definition.type

(type_alias
  name: (type_identifier) @name) @definition.type

(source_file
  (const_declaration
    (const_spec name: (identifier) @name) @definition.const))

(source_file
  (var_declaration
    (var_spec name: (identifier) @name) @definition.var))

(source_file
  (var_declaration
    (var_spec_list
      (var_spec name: (identifier) @name) @definition.var)))

; interface methods
(method_elem
  name: (field_identifier) @name) @definition.method

; ---- references -----------------------------------------------------------
(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (selector_expression
    operand: (_) @qualifier
    field: (field_identifier) @name)) @reference.call

(call_expression
  function: (index_expression
    operand: (identifier) @name)) @reference.call

; `pkg.F[T](a, b)`: explicitly instantiated generic function of another package
(call_expression
  function: (index_expression
    operand: (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name))) @reference.call

; `F[T](x)` / `pkg.F[T](x)` with one argument parse as a conversion to a generic type
(type_conversion_expression
  type: (generic_type
    type: (type_identifier) @name)) @reference.call

(type_conversion_expression
  type: (generic_type
    type: (qualified_type
      package: (package_identifier) @qualifier
      name: (type_identifier) @name))) @reference.call

(composite_literal
  type: (type_identifier) @name) @reference.type

(qualified_type
  package: (package_identifier) @qualifier
  name: (type_identifier) @name) @reference.type

(type_identifier) @name @reference.type

; functions / methods passed as values (handlers, callbacks)
(argument_list
  (selector_expression
    operand: (_) @qualifier
    field: (field_identifier) @name) @reference.value)

(argument_list
  (identifier) @name @reference.value)

; ...and elsewhere: `Skipper: DefaultSkipper`, `[]F{a, b}`, `h = f`, `return f`, `x == f`,
; `e.serveHTTPFunc = e.serveHTTP`. Bare names are kept unless local, selectors only on an
; imported package or a receiver of known type (extract.rs).
(keyed_element
  value: (literal_element
    (identifier) @name @reference.value))

(keyed_element
  value: (literal_element
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(literal_value
  (literal_element
    (identifier) @name @reference.value))

(literal_value
  (literal_element
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(assignment_statement
  right: (expression_list
    (identifier) @name @reference.value))

(assignment_statement
  right: (expression_list
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(short_var_declaration
  right: (expression_list
    (identifier) @name @reference.value))

(short_var_declaration
  right: (expression_list
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(var_spec
  value: (expression_list
    (identifier) @name @reference.value))

(var_spec
  value: (expression_list
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(return_statement
  (expression_list
    (identifier) @name @reference.value))

(return_statement
  (expression_list
    (selector_expression
      operand: (_) @qualifier
      field: (field_identifier) @name) @reference.value))

(binary_expression
  left: (identifier) @name @reference.value)

(binary_expression
  right: (identifier) @name @reference.value)

; `(*ValueSource)(nil)`: a type in expression position
(parenthesized_expression
  (unary_expression
    operator: "*"
    operand: (identifier) @name) @reference.value)

; generic parameters: a capital-letter reference one of these declares in scope is no type
(type_parameter_declaration
  name: (identifier) @name) @generic.param

; ---- imports --------------------------------------------------------------
(import_declaration) @import
