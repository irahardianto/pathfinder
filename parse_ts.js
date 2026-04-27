const Parser = require('tree-sitter');
const TypeScript = require('tree-sitter-typescript').typescript;

const parser = new Parser();
parser.setLanguage(TypeScript);

const sourceCode = 'export namespace Auth { export function login() {} }';
const tree = parser.parse(sourceCode);

console.log(tree.rootNode.toString());
