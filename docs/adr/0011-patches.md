# 0011 — Patches com ids estáveis

## Decisão
O host guarda um `Reconciler` por extensão. Cada árvore nova é comparada com
a anterior — por `key` quando existe, senão por posição e tipo — e só as
diferenças saem, como `Patch`:

| op | conteúdo |
|---|---|
| `create` | id, kind, key, props (sem filhos) |
| `update` | id, props alteradas, props removidas |
| `children` | id, lista completa e ordenada de filhos |
| `root` | novo id raiz |
| `remove` | ids que saíram da árvore |

Patches são dados serde (`{"op":"update","id":7,"set":{"label":"…"}}`) e vão
pela rede como estão quando o modo remoto chegar. `Reconciler::snapshot`
reconstrói a árvore inteira para um renderer que conecta depois. Renderers
locais aplicam com `Trees`/`ViewTree`.

## Consequências
- O GPUI usa o id do nó como `ElementId`: hover e estado de clique
  sobrevivem a re-renders e a hot reloads.
- Um teste de propriedade aplica 500 árvores aleatórias e confere que o
  renderer termina exatamente com a árvore do host, sem vazar nós.
