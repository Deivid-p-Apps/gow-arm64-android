# Performance — metodologia e estado

## Dispositivo de teste (medido via ADB)
- Modelo: Motorola Edge 30 Fusion (`tundra`)
- SoC: Qualcomm SM8350 (Snapdragon 888+)
- GPU: Adreno 725 (Vulkan via Turnip disponível em /storage/emulated/0/god/drivers/)
- RAM: ~7.3 GB (típico ~1.3-1.5 GB livres em uso)
- Android 14
- Tela: 1080x2400, até 144 Hz (modos 60/90/120/144)

## Métricas que serão medidas (quando o jogo rodar)
Por cenário (menu + 1ª área explorável; mundo aberto; combate pesado; cutscene):
mínimo, média, 1% low, p99, máximo de frame time; estado térmico (dumpsys
thermalservice) e clock da GPU (dumpsys gpu / sysfs), repetido após aquecimento.

Fonte dos números: **log de frame time do próprio jogo + Vulkan timestamp queries**,
medidos no dispositivo via ADB. Nada aqui é estimado ou simulado.

## Estado atual
M1: o jogo ainda **não renderiza** (falta D3D11->Vulkan e a swapchain). Portanto
**não há ainda números de FPS a reportar** — reportá-los agora seria inventá-los,
o que este projeto não faz. O que existe medido até agora:
- Recompilador: 6,8 s para liftar as 51.869 funções / 2.754.847 instruções.
- Cobertura do lifter: 99,987%.
- APK arm64 instalado e executado no dispositivo sem crash (lê /storage/emulated/0/god).

## Expectativa honesta de desempenho (a validar, não um resultado)
O teto é a GPU/RAM, não a tradução de CPU. Um port nativo remove o custo do
emulador x86 em tempo de execução, mas a Adreno 725 é um GPU de classe média de
2021. Referências do mesmo jogo em emulador nesta classe de hardware (SD 8 Gen 3,
mais forte) ficam na casa de ~30 fps em 720p/tudo-Low com escala 50%. A meta de
50 fps no preset Alto só poderá ser confirmada ou refutada por medição real; até
lá, qualquer número seria fabricado.
