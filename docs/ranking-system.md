# Ranking System

## Ranking Categories and Points

Each category defines value groups. Each group defines points and a model class range (see model classes below).

work type
- steering=0 (router bypass, exception)
- group1: question/docs_analysis/packaging/operational/testing
  - points=1
  - model range: simple-smart
- group2: implementation/bug_fix/refactor/docs_authoring/orchestration/calibration
  - points=3
  - model range: simple-intelligent
- group3: research/review/diagnosis/design
  - points=6
  - model range: smart-intelligent

risk:
- low
  - points=1
  - model range: simple-smart
- medium
  - points=3
  - model range: simple-intelligent
- high
  - points=10
  - model range: smart-intelligent

complexity:
- low
  - points=1
  - model range: simple-smart
- medium
  - points=3
  - model range: simple-intelligent
- high
  - points=6
  - model range: simple-intelligent
- very_high
  - points=9
  - model range: smart-intelligent

orchestration:
- none
  - points=0
  - model range: simple-smart
- delegate
  - points=1
  - model range: simple-smart
- coordination
  - points=1
  - model range: smart-smart
- workflow
  - points=3
  - model range: smart-smart

current range: 3-35


## Models
- we need model classes: simple, smart, intelligent
- we need to set a minimum and maximum model class per work type category
- group1: min=simple, max=smart
- group2: min=simple, max=intelligent
- group3: min=smart, max=intelligent

now lets say we have the following models:

- gpt-luna, gpt-terra and gpt-sol
- total orderered model lists (pricing order + effort)
- class:simple: 1:luna/low, 2:luna:medium, 3:luna:high, 4:luna/xhigh, 5:luna/max,
  class:smart: 6:terra/low, 7:terra:medium, 8:terra:high, 9:terra/xhigh, 10:terra/max,
  class:intelligent: 11:sol/low, 12:sol:medium, 13:sol:high, 14:sol/xhigh, 15:sol/max

## Routing Decision
1. classification
2. find the model range based on the ranking groups
  - min-class = max(all min ranges)
  - max-class = max(all max ranges)
3. map ranking range to model index range
4. calculate ranking points
5. select model: index = indexMin + ((value - valueMin) / (valueMax - valueMin)) * (indexMax
- indexMin)

## Examples
1. implementation(3) + complexity:medium(3) + risk:medium(3) + orchestration:none(0) = 9
  - work type: group2 -> simple-intelligent
  - complexity:medium -> simple-intelligent
  - risk:medium -> simple-intelligent
  - orchestration:none -> simple-smart
  - => min=simple, max=intelligent -> index range 1-15
  - => ranking=9 -> index=4 -> luna/xhigh

2. research(6) + complexity:medium(3) + risk:high(10) + orchestration:none(0) = 19
  - work type: group3 -> smart-intelligent
  - complexity:medium -> simple-intelligent
  - risk:high -> simple-intelligent
  - orchestration:none -> simple-smart
  - => min=smart, max=intelligent -> index range 6-15
  - => ranking=19 -> index=11 -> sol/low

3. implementation(3) + complexity:medium(3) + risk:medium(3) + orchestration:coordination(1)
= 10
  - work type: group2 -> simple-intelligent
  - complexity:medium -> simple-intelligent
  - risk:medium -> simple-intelligent
  - orchestration:coordination -> smart-smart
  - => min=smart, max=intelligent -> index range 6-15
  - => ranking=10 -> index=8 -> terra/high
