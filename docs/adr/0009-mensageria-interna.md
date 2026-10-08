# 0009 — Mensageria mira rede interna

## Contexto
Uma das ideias é uma mensageria/email que entrega interfaces sandboxed em vez
de HTML. Competir com o email público esbarra em adoção e interoperabilidade.

## Decisão
O alvo é uso em **rede interna** (empresa, casa, laboratório), onde dá para
controlar cliente e servidor. Mensagens renderizam com zero capacidades por
padrão; interações (RSVP, enquete, aprovação) usam ações declarativas que
chamam um serviço do remetente com consentimento.

## Consequências
- Não precisamos de fallback para clientes de email comuns no início.
- Interoperar com email público vira trabalho futuro opcional.
