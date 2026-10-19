# magnetar

A magnetar seen from further out: the blue-black canvas drops to the edge of black while the aurora keeps every volt, so the page is darkness with weather in it.

Canvas `#000406`: J_HK 9.0, M 1.1, h 223.
The text ring stands at J_HK 88.1, held there by syntax.type over ground.canvas.
The signal ring stands at J_HK 80.4, held there by figure.failure over ground.canvas.
The terminal ring stands at J_HK 83.6, held there by slot 5 over ground.canvas.
The label reads at Lc 82.3 over the canvas, ceiling 95; an open tab at Lc 64.6, above the faint ink's 46.3.

| role | hex | J_HK | M | asked | h | xterm |
|---|---|---|---|---|---|---|
| ground.canvas | `#000406` | 9.0 | 1.1 | 3.0 | 223 | 16 |
| ground.trough | `#000102` | 7.3 | 0.5 | 0.9 | 239 | 16 |
| ground.dais | `#050c0f` | 11.6 | 1.4 | 1.4 | 224 | 232 |
| ground.selection | `#001312` | 14.0 | 3.5 | 10.4 | 193 | 233 |
| ground.prompt | `#14151f` | 15.5 | 3.0 | 2.8 | 279 | 233 |
| ground.response | `#000a14` | 11.8 | 2.8 | 9.1 | 245 | 232 |
| ground.reasoning | `#0e0f0e` | 11.8 | 0.5 | 0.4 | 172 | 233 |
| ground.environment | `#0e181c` | 15.6 | 2.0 | 1.9 | 224 | 233 |
| ground.message | `#000715` | 11.5 | 3.7 | 4.2 | 261 | 232 |
| ground.added | `#070a00` | 9.9 | 2.0 | 7.5 | 125 | 232 |
| ground.removed | `#100009` | 10.1 | 3.5 | 7.5 | 346 | 232 |
| figure.ink | `#d7dde1` | 89.2 | 2.6 | 2.5 | 224 | 253 |
| figure.ink_muted | `#b2b7bb` | 74.6 | 2.2 | 2.1 | 227 | 249 |
| figure.ink_faint | `#94989b` | 62.7 | 1.8 | 1.8 | 226 | 246 |
| figure.ink_selected | `#d3d8dd` | 87.3 | 2.5 | 2.5 | 231 | 188 |
| figure.rule | `#4e5356` | 36.7 | 1.6 | 1.8 | 226 | 239 |
| figure.accent | `#00e5ce` | 88.2 | 31.0 | 34.5 | 185 | 43 |
| figure.link | `#00d9f0` | 85.9 | 25.0 | 34.1 | 210 | 45 |
| figure.code | `#d9d46d` | 83.5 | 18.0 | 18.0 | 110 | 185 |
| figure.success | `#48d28c` | 79.1 | 29.9 | 30.0 | 160 | 78 |
| figure.caution | `#e5b600` | 76.5 | 26.5 | 30.0 | 92 | 178 |
| figure.failure | `#ff696b` | 69.7 | 29.9 | 30.0 | 20 | 203 |
| figure.info | `#00aff7` | 73.9 | 24.7 | 30.0 | 240 | 39 |
| figure.added | `#bde838` | 87.3 | 29.9 | 30.0 | 125 | 148 |
| figure.removed | `#ff8bd4` | 79.4 | 26.2 | 30.0 | 345 | 212 |
| figure.label | `#bdded6` | 89.0 | 9.2 | 9.0 | 185 | 152 |
| figure.tab | `#85c5b9` | 78.7 | 15.1 | 15.0 | 185 | 116 |
| syntax.keyword | `#00e5ce` | 88.2 | 31.0 | 34.5 | 185 | 43 |
| syntax.function | `#d794ee` | 77.4 | 26.9 | 26.8 | 320 | 176 |
| syntax.type | `#89a8ff` | 76.5 | 26.9 | 30.4 | 270 | 111 |
| syntax.string | `#e6e100` | 88.0 | 29.5 | 31.2 | 110 | 184 |
| syntax.number | `#ff9c88` | 77.7 | 19.6 | 25.9 | 30 | 216 |
| syntax.comment | `#609ab6` | 64.5 | 12.0 | 12.0 | 230 | 74 |
| syntax.punctuation | `#a3afb8` | 72.0 | 3.9 | 3.8 | 233 | 145 |
| syntax.variable | `#bbd1de` | 85.1 | 6.0 | 6.0 | 229 | 152 |
| syntax.constant | `#ff9c88` | 77.7 | 19.6 | 25.9 | 30 | 216 |
| syntax.attribute | `#00d6f6` | 85.2 | 24.3 | 33.9 | 215 | 45 |

## surfaces

| surface | spec | doctrine |
|---|---|---|
| trough | -2.5 | -3 |
| dais | 4 | 5 |
| selection rise | 7.5 | 9 |
| diff tint | 7.5 | 7 |
| response | 4 | 3 |

## speakers

| ground | corner | hue | rise | tint | dE off canvas |
|---|---|---|---|---|---|
| ground.response | agent | 248 | 4 | 7.75 | 6.5 |
| ground.message | agent | 275 | 3.5 | 3.20 | 8.4 |
| ground.prompt | user | 320 | 9.5 | 2.20 | 11.1 |
| ground.reasoning | cot | 60 | 4 | 1.20 | 7.2 |
| ground.environment | environment | grey | 9.5 | 0.00 | 9.2 |

- off the canvas: least dE 6.5 (ground.response), floor 6.5, the response's 6
- corners: least dE 7.4 (ground.prompt and ground.environment), floor 6.5
- agent corner: ground.response and ground.message stand dE 3.3 apart, between 3 and 5
- loudness: the response at dE 6.5 and the prompt at dE 11.1; the rest between 7.2 and 9.2

## contrast

| text | ground | Lc | floor |
|---|---|---|---|
| figure.ink | ground.canvas | 85.4 | 85 |
| figure.ink | ground.trough | 85.4 | 83 |
| figure.ink | ground.dais | 85.2 | 81 |
| figure.ink | ground.selection | 84.9 | 75 |
| figure.ink | ground.prompt | 84.5 | 81 |
| figure.ink | ground.response | 85.3 | 81 |
| figure.ink | ground.reasoning | 85.1 | 81 |
| figure.ink | ground.environment | 84.4 | 81 |
| figure.ink | ground.message | 85.3 | 81 |
| figure.ink | ground.added | 85.3 | 80 |
| figure.ink | ground.removed | 85.3 | 80 |
| figure.ink_muted | ground.canvas | 63.0 | 63 |
| figure.ink_muted | ground.trough | 63.0 | 61 |
| figure.ink_muted | ground.dais | 62.8 | 59 |
| figure.ink_muted | ground.selection | 62.5 | 53 |
| figure.ink_muted | ground.prompt | 62.1 | 59 |
| figure.ink_muted | ground.response | 62.9 | 59 |
| figure.ink_muted | ground.reasoning | 62.7 | 59 |
| figure.ink_muted | ground.environment | 62.1 | 59 |
| figure.ink_muted | ground.message | 62.9 | 59 |
| figure.ink_muted | ground.added | 62.9 | 58 |
| figure.ink_muted | ground.removed | 62.9 | 58 |
| figure.ink_faint | ground.canvas | 46.3 | 46 |
| figure.ink_faint | ground.trough | 46.4 | 44 |
| figure.ink_faint | ground.dais | 46.2 | 42 |
| figure.ink_faint | ground.selection | 45.8 | 36 |
| figure.ink_faint | ground.prompt | 45.5 | 42 |
| figure.ink_faint | ground.response | 46.2 | 42 |
| figure.ink_faint | ground.reasoning | 46.0 | 42 |
| figure.ink_faint | ground.environment | 45.4 | 42 |
| figure.ink_faint | ground.message | 46.3 | 42 |
| figure.ink_faint | ground.added | 46.2 | 41 |
| figure.ink_faint | ground.removed | 46.3 | 41 |
| figure.ink_selected | ground.selection | 82.0 | 82 |
| figure.rule | ground.canvas | 15.0 | 15 |
| figure.rule | ground.dais | 14.8 | 8 |
| figure.rule | ground.response | 14.9 | 11 |
| figure.rule | ground.reasoning | 14.7 | 11 |
| figure.rule | ground.message | 14.9 | 11 |
| figure.accent | ground.canvas | 76.6 | 55 |
| figure.accent | ground.trough | 76.6 | 53 |
| figure.accent | ground.dais | 76.4 | 51 |
| figure.accent | ground.selection | 76.1 | 45 |
| figure.accent | ground.prompt | 75.7 | 51 |
| figure.accent | ground.response | 76.4 | 51 |
| figure.accent | ground.reasoning | 76.2 | 51 |
| figure.accent | ground.environment | 75.6 | 51 |
| figure.accent | ground.message | 76.5 | 51 |
| figure.link | ground.canvas | 72.4 | 55 |
| figure.link | ground.trough | 72.4 | 53 |
| figure.link | ground.dais | 72.2 | 51 |
| figure.link | ground.selection | 71.9 | 45 |
| figure.link | ground.prompt | 71.5 | 51 |
| figure.link | ground.response | 72.2 | 51 |
| figure.link | ground.reasoning | 72.0 | 51 |
| figure.link | ground.environment | 71.4 | 51 |
| figure.link | ground.message | 72.3 | 51 |
| figure.link | ground.added | 72.3 | 51 |
| figure.link | ground.removed | 72.3 | 51 |
| figure.code | ground.canvas | 78.0 | 78 |
| figure.code | ground.trough | 78.0 | 78 |
| figure.code | ground.dais | 77.8 | 74 |
| figure.code | ground.prompt | 77.1 | 74 |
| figure.code | ground.response | 77.9 | 74 |
| figure.code | ground.reasoning | 77.7 | 74 |
| figure.code | ground.environment | 77.1 | 74 |
| figure.code | ground.message | 77.9 | 74 |
| figure.code | ground.added | 77.9 | 74 |
| figure.code | ground.removed | 77.9 | 74 |
| figure.success | ground.canvas | 65.8 | 47 |
| figure.success | ground.trough | 65.8 | 45 |
| figure.success | ground.dais | 65.7 | 43 |
| figure.success | ground.selection | 65.3 | 37 |
| figure.success | ground.prompt | 65.0 | 43 |
| figure.success | ground.response | 65.7 | 43 |
| figure.success | ground.reasoning | 65.5 | 43 |
| figure.success | ground.environment | 64.9 | 43 |
| figure.success | ground.message | 65.7 | 43 |
| figure.caution | ground.canvas | 66.4 | 47 |
| figure.caution | ground.trough | 66.5 | 45 |
| figure.caution | ground.dais | 66.3 | 43 |
| figure.caution | ground.selection | 65.9 | 37 |
| figure.caution | ground.prompt | 65.6 | 43 |
| figure.caution | ground.response | 66.3 | 43 |
| figure.caution | ground.reasoning | 66.1 | 43 |
| figure.caution | ground.environment | 65.5 | 43 |
| figure.caution | ground.message | 66.3 | 43 |
| figure.caution | ground.added | 66.3 | 43 |
| figure.caution | ground.removed | 66.3 | 43 |
| figure.failure | ground.canvas | 48.7 | 47 |
| figure.failure | ground.trough | 48.7 | 45 |
| figure.failure | ground.dais | 48.5 | 43 |
| figure.failure | ground.selection | 48.2 | 37 |
| figure.failure | ground.prompt | 47.8 | 43 |
| figure.failure | ground.response | 48.5 | 43 |
| figure.failure | ground.reasoning | 48.3 | 43 |
| figure.failure | ground.environment | 47.7 | 43 |
| figure.failure | ground.message | 48.6 | 43 |
| figure.failure | ground.added | 48.6 | 43 |
| figure.failure | ground.removed | 48.6 | 43 |
| figure.info | ground.canvas | 54.0 | 47 |
| figure.info | ground.trough | 54.0 | 45 |
| figure.info | ground.dais | 53.8 | 43 |
| figure.info | ground.selection | 53.5 | 37 |
| figure.info | ground.prompt | 53.1 | 43 |
| figure.info | ground.response | 53.9 | 43 |
| figure.info | ground.reasoning | 53.7 | 43 |
| figure.info | ground.environment | 53.0 | 43 |
| figure.info | ground.message | 53.9 | 43 |
| figure.added | ground.canvas | 83.3 | 55 |
| figure.added | ground.trough | 83.3 | 55 |
| figure.added | ground.response | 83.1 | 51 |
| figure.added | ground.reasoning | 82.9 | 51 |
| figure.added | ground.message | 83.2 | 51 |
| figure.added | ground.added | 83.2 | 55 |
| figure.added | ground.removed | 83.2 | 51 |
| figure.removed | ground.canvas | 61.0 | 55 |
| figure.removed | ground.trough | 61.0 | 55 |
| figure.removed | ground.response | 60.9 | 51 |
| figure.removed | ground.reasoning | 60.7 | 51 |
| figure.removed | ground.message | 60.9 | 51 |
| figure.removed | ground.added | 60.9 | 51 |
| figure.removed | ground.removed | 60.9 | 55 |
| figure.label | ground.canvas | 82.3 | 82 |
| figure.label | ground.trough | 82.3 | 80 |
| figure.label | ground.dais | 82.1 | 78 |
| figure.label | ground.selection | 81.8 | 72 |
| figure.label | ground.prompt | 81.4 | 78 |
| figure.label | ground.response | 82.2 | 78 |
| figure.label | ground.reasoning | 81.9 | 78 |
| figure.label | ground.environment | 81.3 | 78 |
| figure.label | ground.message | 82.2 | 78 |
| figure.tab | ground.canvas | 64.6 | 64 |
| syntax.keyword | ground.canvas | 76.6 | 55 |
| syntax.keyword | ground.trough | 76.6 | 55 |
| syntax.keyword | ground.response | 76.4 | 51 |
| syntax.keyword | ground.reasoning | 76.2 | 51 |
| syntax.keyword | ground.message | 76.5 | 51 |
| syntax.keyword | ground.added | 76.5 | 51 |
| syntax.keyword | ground.removed | 76.5 | 51 |
| syntax.function | ground.canvas | 58.1 | 55 |
| syntax.function | ground.trough | 58.1 | 55 |
| syntax.function | ground.response | 57.9 | 51 |
| syntax.function | ground.reasoning | 57.7 | 51 |
| syntax.function | ground.message | 58.0 | 51 |
| syntax.function | ground.added | 58.0 | 51 |
| syntax.function | ground.removed | 58.0 | 51 |
| syntax.type | ground.canvas | 56.7 | 55 |
| syntax.type | ground.trough | 56.7 | 55 |
| syntax.type | ground.response | 56.6 | 51 |
| syntax.type | ground.reasoning | 56.4 | 51 |
| syntax.type | ground.message | 56.6 | 51 |
| syntax.type | ground.added | 56.6 | 51 |
| syntax.type | ground.removed | 56.6 | 51 |
| syntax.string | ground.canvas | 84.9 | 55 |
| syntax.string | ground.trough | 84.9 | 55 |
| syntax.string | ground.response | 84.8 | 51 |
| syntax.string | ground.reasoning | 84.6 | 51 |
| syntax.string | ground.message | 84.8 | 51 |
| syntax.string | ground.added | 84.8 | 51 |
| syntax.string | ground.removed | 84.8 | 51 |
| syntax.number | ground.canvas | 63.2 | 55 |
| syntax.number | ground.trough | 63.2 | 55 |
| syntax.number | ground.response | 63.1 | 51 |
| syntax.number | ground.reasoning | 62.9 | 51 |
| syntax.number | ground.message | 63.1 | 51 |
| syntax.number | ground.added | 63.1 | 51 |
| syntax.number | ground.removed | 63.1 | 51 |
| syntax.comment | ground.canvas | 44.0 | 41 |
| syntax.comment | ground.trough | 44.0 | 44 |
| syntax.comment | ground.response | 43.8 | 41 |
| syntax.comment | ground.reasoning | 43.6 | 41 |
| syntax.comment | ground.message | 43.9 | 41 |
| syntax.comment | ground.added | 43.9 | 41 |
| syntax.comment | ground.removed | 43.9 | 41 |
| syntax.punctuation | ground.canvas | 58.0 | 58 |
| syntax.punctuation | ground.trough | 58.0 | 58 |
| syntax.punctuation | ground.response | 57.9 | 58 |
| syntax.punctuation | ground.reasoning | 57.7 | 58 |
| syntax.punctuation | ground.message | 57.9 | 58 |
| syntax.punctuation | ground.added | 57.9 | 58 |
| syntax.punctuation | ground.removed | 57.9 | 58 |
| syntax.variable | ground.canvas | 76.6 | 76 |
| syntax.variable | ground.trough | 76.6 | 76 |
| syntax.variable | ground.response | 76.5 | 76 |
| syntax.variable | ground.reasoning | 76.3 | 76 |
| syntax.variable | ground.message | 76.5 | 76 |
| syntax.variable | ground.added | 76.5 | 76 |
| syntax.variable | ground.removed | 76.5 | 76 |
| syntax.constant | ground.canvas | 63.2 | 55 |
| syntax.constant | ground.trough | 63.2 | 55 |
| syntax.constant | ground.response | 63.1 | 51 |
| syntax.constant | ground.reasoning | 62.9 | 51 |
| syntax.constant | ground.message | 63.1 | 51 |
| syntax.constant | ground.added | 63.1 | 51 |
| syntax.constant | ground.removed | 63.1 | 51 |
| syntax.attribute | ground.canvas | 71.3 | 55 |
| syntax.attribute | ground.trough | 71.3 | 55 |
| syntax.attribute | ground.response | 71.1 | 51 |
| syntax.attribute | ground.reasoning | 70.9 | 51 |
| syntax.attribute | ground.message | 71.2 | 51 |
| syntax.attribute | ground.added | 71.2 | 51 |
| syntax.attribute | ground.removed | 71.2 | 51 |
| livery 0 | ground.canvas | 82.8 | 55 |
| livery 1 | ground.canvas | 75.3 | 55 |
| livery 2 | ground.canvas | 82.3 | 55 |
| livery 3 | ground.canvas | 66.8 | 55 |
| livery 4 | ground.canvas | 81.8 | 55 |
| livery 5 | ground.canvas | 65.9 | 55 |
| livery 6 | ground.canvas | 62.2 | 55 |
| livery 7 | ground.canvas | 86.4 | 55 |
| livery 0 | ground.trough | 82.9 | 53 |
| livery 1 | ground.trough | 75.3 | 53 |
| livery 2 | ground.trough | 82.4 | 53 |
| livery 3 | ground.trough | 66.8 | 53 |
| livery 4 | ground.trough | 81.8 | 53 |
| livery 5 | ground.trough | 65.9 | 53 |
| livery 6 | ground.trough | 62.2 | 53 |
| livery 7 | ground.trough | 86.4 | 53 |
| livery 0 | ground.dais | 82.7 | 51 |
| livery 1 | ground.dais | 75.1 | 51 |
| livery 2 | ground.dais | 82.2 | 51 |
| livery 3 | ground.dais | 66.6 | 51 |
| livery 4 | ground.dais | 81.6 | 51 |
| livery 5 | ground.dais | 65.7 | 51 |
| livery 6 | ground.dais | 62.0 | 51 |
| livery 7 | ground.dais | 86.2 | 51 |
| heat 1 | ground.canvas | 15.3 | 15 |
| heat 2 | ground.canvas | 32.4 | 15 |
| heat 3 | ground.canvas | 53.1 | 15 |
| heat 4 | ground.canvas | 76.6 | 15 |
| terminal 1 | ground.canvas | 54.1 | 47 |
| terminal 2 | ground.canvas | 70.9 | 47 |
| terminal 3 | ground.canvas | 71.2 | 47 |
| terminal 4 | ground.canvas | 58.2 | 47 |
| terminal 5 | ground.canvas | 49.0 | 47 |
| terminal 6 | ground.canvas | 69.5 | 47 |
| terminal 9 | ground.canvas | 68.1 | 47 |
| terminal 10 | ground.canvas | 83.3 | 47 |
| terminal 11 | ground.canvas | 83.9 | 47 |
| terminal 12 | ground.canvas | 72.4 | 47 |
| terminal 13 | ground.canvas | 60.2 | 47 |
| terminal 14 | ground.canvas | 81.3 | 47 |
| terminal 1 | ground.prompt | 53.2 | 43 |
| terminal 2 | ground.prompt | 70.0 | 43 |
| terminal 3 | ground.prompt | 70.3 | 43 |
| terminal 4 | ground.prompt | 57.3 | 43 |
| terminal 5 | ground.prompt | 48.1 | 43 |
| terminal 6 | ground.prompt | 68.6 | 43 |
| terminal 9 | ground.prompt | 67.2 | 43 |
| terminal 10 | ground.prompt | 82.4 | 43 |
| terminal 11 | ground.prompt | 83.0 | 43 |
| terminal 12 | ground.prompt | 71.5 | 43 |
| terminal 13 | ground.prompt | 59.3 | 43 |
| terminal 14 | ground.prompt | 80.4 | 43 |
| terminal 1 | ground.response | 53.9 | 43 |
| terminal 2 | ground.response | 70.7 | 43 |
| terminal 3 | ground.response | 71.1 | 43 |
| terminal 4 | ground.response | 58.1 | 43 |
| terminal 5 | ground.response | 48.9 | 43 |
| terminal 6 | ground.response | 69.3 | 43 |
| terminal 9 | ground.response | 68.0 | 43 |
| terminal 10 | ground.response | 83.2 | 43 |
| terminal 11 | ground.response | 83.8 | 43 |
| terminal 12 | ground.response | 72.2 | 43 |
| terminal 13 | ground.response | 60.1 | 43 |
| terminal 14 | ground.response | 81.2 | 43 |
| terminal 1 | ground.environment | 53.1 | 43 |
| terminal 2 | ground.environment | 69.9 | 43 |
| terminal 3 | ground.environment | 70.2 | 43 |
| terminal 4 | ground.environment | 57.2 | 43 |
| terminal 5 | ground.environment | 48.0 | 43 |
| terminal 6 | ground.environment | 68.5 | 43 |
| terminal 9 | ground.environment | 67.1 | 43 |
| terminal 10 | ground.environment | 82.3 | 43 |
| terminal 11 | ground.environment | 82.9 | 43 |
| terminal 12 | ground.environment | 71.4 | 43 |
| terminal 13 | ground.environment | 59.2 | 43 |
| terminal 14 | ground.environment | 80.3 | 43 |
| terminal 1 | ground.message | 54.0 | 43 |
| terminal 2 | ground.message | 70.8 | 43 |
| terminal 3 | ground.message | 71.1 | 43 |
| terminal 4 | ground.message | 58.1 | 43 |
| terminal 5 | ground.message | 48.9 | 43 |
| terminal 6 | ground.message | 69.4 | 43 |
| terminal 9 | ground.message | 68.0 | 43 |
| terminal 10 | ground.message | 83.2 | 43 |
| terminal 11 | ground.message | 83.8 | 43 |
| terminal 12 | ground.message | 72.3 | 43 |
| terminal 13 | ground.message | 60.1 | 43 |
| terminal 14 | ground.message | 81.2 | 43 |

## distinctness

- status: least dE 34.5, floor 14
- accent against status: least dE 13.9, floor 10
- diff ink: least dE 61.9, floor 18
- diff band: least dE 19.9, floor 6
- syntax: least dE 15.8, floor 9
- code recession: least dE 9.4, floor 5
- surfaces: least dE 3.3, floor 2
- ink ladder: least dE 9.3, floor 8
- code against prose: least dE 26.8, floor 10
- terminal: least dE 18.1, floor 10
- terminal brights: least dE 18.1, floor 10
- terminal brights from their slots: least dE 5.3 (slot 6), floor 5
- heat over ground.canvas: least step dE 11.9 (into step 4), floor 5
- livery: least dE 18.5 (entries 5 and 6); nearest status 18.5 reach-scaled dE (entry 6, figure.failure)

| livery | hex | J_HK | M | asked | h | xterm |
|---|---|---|---|---|---|---|
| 0 | `#ffd093` | 87.1 | 15.0 | 36.0 | 75 | 222 |
| 1 | `#b7d900` | 82.2 | 30.1 | 36.0 | 122 | 148 |
| 2 | `#00efdf` | 92.2 | 31.4 | 36.0 | 189 | 51 |
| 3 | `#00ccf8` | 82.2 | 23.4 | 36.0 | 223 | 45 |
| 4 | `#bed8ff` | 90.0 | 12.3 | 36.0 | 255 | 153 |
| 5 | `#c8aeff` | 82.1 | 25.0 | 36.0 | 300 | 183 |
| 6 | `#fe87ff` | 82.1 | 36.0 | 36.0 | 331 | 213 |
| 7 | `#ffd2e0` | 91.0 | 7.5 | 36.0 | 355 | 224 |

| heat | hex | J_HK | M | asked | h | xterm |
|---|---|---|---|---|---|---|
| 1 | `#375952` | 38.8 | 8.0 | 7.8 | 183 | 23 |
| 2 | `#39877b` | 55.3 | 15.6 | 15.5 | 185 | 30 |
| 3 | `#31b6a4` | 71.8 | 23.4 | 23.3 | 184 | 37 |
| 4 | `#00e5ce` | 88.2 | 31.0 | 34.5 | 185 | 43 |

| terminal | hex | J_HK | M | asked | h | xterm |
|---|---|---|---|---|---|---|
| 0 | `#050c0f` | 11.6 | 1.4 | 1.4 | 224 | 232 |
| 1 | `#ff7e7c` | 72.9 | 25.3 | 30.0 | 20 | 210 |
| 2 | `#55db94` | 82.4 | 30.1 | 30.0 | 160 | 78 |
| 3 | `#efbe00` | 79.7 | 27.6 | 30.0 | 92 | 178 |
| 4 | `#29b7ff` | 77.0 | 24.6 | 30.0 | 240 | 39 |
| 5 | `#b986ea` | 71.5 | 30.1 | 30.0 | 310 | 140 |
| 6 | `#00d6d3` | 83.8 | 27.1 | 30.0 | 195 | 44 |
| 7 | `#b2b7bb` | 74.6 | 2.2 | 2.1 | 227 | 249 |
| 8 | `#94989b` | 62.7 | 1.8 | 1.8 | 226 | 246 |
| 9 | `#ffa8a5` | 80.8 | 15.8 | 30.0 | 20 | 217 |
| 10 | `#72f0a9` | 90.4 | 30.0 | 30.0 | 160 | 121 |
| 11 | `#ffd55d` | 87.6 | 22.7 | 30.0 | 92 | 221 |
| 12 | `#88ceff` | 85.2 | 17.1 | 30.0 | 240 | 117 |
| 13 | `#cf9bff` | 79.4 | 30.1 | 30.0 | 310 | 183 |
| 14 | `#00ece9` | 91.7 | 29.6 | 30.0 | 195 | 51 |
| 15 | `#d7dde1` | 89.2 | 2.6 | 2.5 | 224 | 253 |

## faults

- none

## advisories

- figure.accent yields M 34.5 to 31.0 at the edge of the gamut
- figure.link yields M 34.1 to 25.0 at the edge of the gamut
- figure.caution yields M 30.0 to 26.5 at the edge of the gamut
- figure.info yields M 30.0 to 24.7 at the edge of the gamut
- figure.removed yields M 30.0 to 26.2 at the edge of the gamut
- syntax.keyword yields M 34.5 to 31.0 at the edge of the gamut
- syntax.type yields M 30.4 to 26.9 at the edge of the gamut
- syntax.string yields M 31.2 to 29.5 at the edge of the gamut
- syntax.number yields M 25.9 to 19.6 at the edge of the gamut
- syntax.constant yields M 25.9 to 19.6 at the edge of the gamut
- syntax.attribute yields M 33.9 to 24.3 at the edge of the gamut
