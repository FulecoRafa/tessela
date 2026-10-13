# 0015 — Projetos: entradas, pacotes, superfícies e primitivos estilizados

## Contexto
A marathon tinha um widget por pasta, um arquivo cada, desenhado pela paleta
do renderer. Reconstruir uma shell real (a config `fuleco` do Quickshell)
pediu o modelo de um app React: uma *página* por janela, uma biblioteca de
componentes compartilhada, o visual inteiro nos componentes.

## Decisão
**Projeto** é uma pasta com `tessela.toml`. Cada `[surfaces.<nome>]` é uma
*entrada*: um arquivo rodado como extensão própria, com suas capacidades e
limites. `tessela run <pasta>` monta o app (crate `tessela-cli`).

**Imports** existem só de dois jeitos, resolvidos pelo core e lidos pelo host
(`Sources`), nunca pelo engine:
- relativos (`./x`, `../x`), presos à raiz de quem importa;
- pacotes (`@kit/x`), raízes que o projeto declara em `[imports]`, com o
  mesmo confinamento. Nome não declarado não existe.

**Superfície**: a raiz de uma entrada é um `Surface` (camada, âncoras,
tamanho, margens, zona exclusiva, `visible`, `onDismiss`). O renderer abre uma
janela por superfície visível e uma por `Popup` aberto.

**Primitivos estilizados** no vocabulário padrão (não no `CORE`): `Box`,
`Label`, `Image`, `Ring`, `Popup`, `Input`, `Surface`. Estilo é dado validado,
não linguagem: cores hex ou tokens de tema (`"$surface"`), comprimentos em px
ou `"N%"`, lados e raios como número ou lista fixa. Cada um tem fallback em
componentes do núcleo (ADR 0003): o terminal continua desenhando tudo.

**Tema**: tokens por esquema (`[theme.dark]`, `[theme.light]`, ou arquivos
em `themes = [...]`), resolvidos pelo renderer conforme `ctx.colorScheme()`.

## Consequências
- Uma biblioteca de componentes pode viver fora de qualquer app e servir a
  vários (a shell e um launcher usam o mesmo `@kit`).
- `Ring` é um elemento fechado, não um `Canvas` livre (ADR 0003 continua
  valendo para fingerprinting).
- `Image` só mostra o que o host resolve: `asset:` (projeto), `icon:` (tema
  de ícones) e `file:` sob diretórios permitidos.
- `h` deixou de ser a factory do JSX transpilado (`__jsx`): uma variável `h`
  do guest a sombreava.
