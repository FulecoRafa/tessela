# 0006 — Pontos de extensão sob demanda

## Contexto
O framework deve aceitar extensões que não desenham (providers de busca,
comandos, serviços escritos no guest). Desenhar esse contrato sem um app que
o use tende a errar.

## Decisão
O manifesto já tem `kind` (padrão `"view"`), e o host recusa qualquer outro
valor por enquanto. O contrato de pontos de extensão será desenhado na fase
2, junto com o launcher, que é o primeiro app que precisa dele.

## Consequências
- Nenhuma API especulativa para manter.
- Manifestos atuais continuam válidos quando novos `kind`s chegarem.
