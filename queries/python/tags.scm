; codegraph tags for Python (see go/tags.scm for capture conventions).

; ---- definitions ----------------------------------------------------------
(class_definition
  name: (identifier) @name) @definition.class

(function_definition
  name: (identifier) @name) @definition.function

(module
  (expression_statement
    (assignment
      left: (identifier) @name) @definition.const))

; ---- references -----------------------------------------------------------
(call
  function: (identifier) @name) @reference.call

(call
  function: (attribute
    object: (_) @qualifier
    attribute: (identifier) @name)) @reference.call

(class_definition
  superclasses: (argument_list
    (identifier) @name)) @reference.inherit

(class_definition
  superclasses: (argument_list
    (attribute
      object: (_) @qualifier
      attribute: (identifier) @name))) @reference.inherit

(type
  (identifier) @name) @reference.type

; callables passed as values
(argument_list
  (attribute
    object: (_) @qualifier
    attribute: (identifier) @name) @reference.value)

(argument_list
  (identifier) @name @reference.value)

; ---- imports --------------------------------------------------------------
(import_statement) @import
(import_from_statement) @import
