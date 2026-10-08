# 0004 — Capacidades no manifesto

## Decisão
O manifesto declara `capabilities = ["service:power", "timer:*"]`. O formato
é `tipo:recurso`; `tipo:*` concede todos os recursos do tipo. Quem checa é o
módulo, via `ModuleCx::require`, no momento da chamada. `uses = ["power"]`
continua aceito como atalho para `service:power`.

## Consequências
- Um módulo novo traz seu próprio tipo de capacidade sem mudar o core.
- Fase 1 adiciona quotas (chamadas/s, assinaturas) no mesmo lugar.
- Fase 4 (código de terceiros) adiciona prompts ao usuário e diff de
  capacidades em atualizações.
