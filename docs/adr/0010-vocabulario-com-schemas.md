# 0010 — Nós genéricos validados por schema

## Contexto
Na fase 0 o vocabulário era um `enum Node` com uma variante por componente.
Isso deu exaustividade aos renderers, mas impedia apps de registrar
componentes próprios e espalhava `key` por todas as variantes.

## Decisão
`Node { kind, key, props, children }`. Cada componente tem um `ComponentSpec`:
props tipadas (`String`, `Number`, `Bool`, `Tone`, `Handler`), obrigatórias ou
não, e se aceita filhos. O host valida toda árvore de guest contra o
`Vocabulary`: componente desconhecido, prop desconhecida, tipo errado ou prop
obrigatória faltando viram um nó de erro. `Error` é construído só pelo host.

Apps registram componentes com `AppBuilder::component`, sempre com um
*fallback* em componentes existentes. O renderer declara o que desenha
(`Renderer::supports`); o resto chega já reescrito pelo fallback.

## Consequências
- A regra da ADR 0003 continua valendo para o núcleo; apps crescem o próprio
  vocabulário sem pedir nada ao framework.
- Renderers trocam `match` exaustivo por `match` em `kind` com um caso padrão.
- `key` vale para qualquer componente.
