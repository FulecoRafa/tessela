# 0003 — Vocabulário fechado, mínimo e voltado a UI

## Contexto
O DOM nasceu para documentos de texto e cresceu até virar uma plataforma
inteira. Um vocabulário fechado é o que torna o sandbox seguro e permite
vários renderers, mas existe a pressão natural para crescer.

## Decisão
O vocabulário é pensado para **interfaces**, não documentos, e fica o menor
possível. Regras para um componente entrar:
1. um app real precisa dele;
2. ele tem um *fallback* expresso em componentes existentes;
3. pelo menos dois renderers o implementam.

Hoje: `Row`, `Column`, `Text`, `Icon`, `Pill`, `Button`, `Progress`, `Spacer`
(lista em `tessela_view::COMPONENTS`). Na fase 1, apps poderão registrar
componentes próprios sob as mesmas regras.

## Consequências
- Guests não conseguem pedir nada que um renderer não saiba desenhar.
- Haverá pedidos por um `Canvas` livre; ele reabre fingerprinting e precisa de
  um ADR próprio.
