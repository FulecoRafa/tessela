# 0013 — Pool de workers

## Decisão
Em vez de uma thread por extensão, um pool pequeno (padrão: núcleos, no
máximo 4; `AppBuilder::workers`). Cada extensão fica presa a um worker a vida
toda (engines não são `Send`). O worker processa tudo que chegou e só então
renderiza as extensões que ficaram sujas, uma vez cada.

## Consequências
- 50 extensões custam 4 threads, não 50.
- Uma extensão desgovernada atrasa as vizinhas do mesmo worker em no máximo
  um orçamento de CPU (50 ms por padrão) antes de ser interrompida.
