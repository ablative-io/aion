((doc_comment) @comment.documentation
 (#match? @comment.documentation "^///"))

((doc_comment) @comment.documentation.workflow
 (#match? @comment.documentation.workflow "^//!"))

(comment) @comment
(string) @string
(duration) @number
(float) @number.float
(integer) @number
(boolean) @constant.builtin
(builtin_type) @type.builtin
(type_identifier) @type

(workflow_declaration name: (identifier) @function)
(input_declaration name: (identifier) @variable.parameter)
(signal_declaration name: (identifier) @variable.parameter)
(outcome_declaration name: (identifier) @label)
(type_declaration name: (type_identifier) @type.definition)
(worker_declaration name: (identifier) @namespace)
(action_declaration name: (identifier) @function.method)
(child_declaration name: (identifier) @function)
(step_declaration name: (identifier) @function)

(keyword) @keyword

((keyword) @function.builtin
 (#any-of? @function.builtin "filter" "map" "sort" "count"))

(pipe_operator) @operator
(bind_operator) @operator
(comparison_operator) @operator
(optional_operator) @operator
