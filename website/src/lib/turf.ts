// 1-bit turf tiles in the style of the Hypercard Library nature set: 16px wide, blades over a solid base.
// Every tile shares the same base rows so any sequence of them joins into one strip.
const TILES = [
  [
    '.#...........#..',
    '.#..........##..',
    '.##.....#..##...',
    '..##...##.##...#',
    '#.##..##..##..##',
    '#..##.##.##..##.',
    '##.#####.###.###',
  ],
  [
    '................',
    '.....#........#.',
    '.#...#.......##.',
    '.#...##..#...#..',
    '.##...#..#..##..',
    '..##..##.##.#..#',
    '#.###.#####.####',
  ],
  [
    '.......#........',
    '.......#........',
    '#......##....#..',
    '#.......#....#..',
    '##..#...##..##..',
    '.#..##..##..#.#.',
    '.##.###.###.####',
  ],
  [
    '..........#.....',
    '...#.......#....',
    '...#.......##...',
    '..##...#....#..#',
    '..#....##...##.#',
    '.##.#...#.#..##.',
    '###.###.#####.##',
  ],
  [
    '................',
    '................',
    '................',
    '..#.......#.....',
    '..##..#...##..#.',
    '#..#..##...#.##.',
    '##.###.##.######',
  ],
].map((blades) => [...blades, '################', '################', '################'])

export const TILE_SIZE = 16
export const TILE_HEIGHT = TILES[0].length

// Scrambled so no tile sits next to itself; long enough to span the widest footer at 2x.
const STRIP = [0, 3, 1, 4, 2, 0, 3, 1, 2, 4, 0, 1, 3, 2, 0, 4, 1, 3, 0, 2, 4, 3, 1, 0]

const tilePath = (tile: string[], dx: number) =>
  tile
    .flatMap((row, y) =>
      [...row.matchAll(/#+/g)].map((run) => `M${dx + (run.index ?? 0)} ${y}h${run[0].length}v1h-${run[0].length}z`),
    )
    .join('')

// The favicon is the first tile, centered vertically in a square.
export const iconSvg = (fill: string, style = '') => {
  const top = (TILE_SIZE - TILE_HEIGHT) / 2
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -${top} ${TILE_SIZE} ${TILE_SIZE}" shape-rendering="crispEdges" fill="${fill}">${style}<path d="${tilePath(TILES[0], 0)}"/></svg>`
}

export const stripSvg = () => {
  const width = STRIP.length * TILE_SIZE
  const path = STRIP.map((tile, i) => tilePath(TILES[tile], i * TILE_SIZE)).join('')
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${TILE_HEIGHT}" viewBox="0 0 ${width} ${TILE_HEIGHT}" shape-rendering="crispEdges"><path d="${path}"/></svg>`
}
