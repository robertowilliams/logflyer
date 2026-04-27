/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{vue,js,ts,jsx,tsx}'],
  theme: {
    extend: {
      colors: {
        // Primary: Crimson (VectaDB brand red)
        primary: {
          50:  '#fff0f3',
          100: '#ffd6df',
          200: '#ffadbf',
          300: '#ff6b8a',
          400: '#ff1493',   // hot pink — hover accent
          500: '#e8173f',
          600: '#dc143c',   // main crimson
          700: '#b8102f',
          800: '#8b0000',   // deep red
          900: '#5c0010',
        },
        // Secondary: Cyan (VectaDB electric blue)
        cyan: {
          400: '#00d4ff',
          500: '#00b8d9',
          600: '#0090ad',
        },
        // Surfaces: near-black backgrounds
        surface: {
          950: '#0a0a0a',   // page background
          900: '#0f0f0f',   // sidebar / cards
          800: '#141414',   // elevated card
          700: '#1a1a1a',   // input / table row hover
          600: '#222222',   // border, divider
        },
        // Text: beige/cream (VectaDB brand text)
        cream: {
          DEFAULT: '#f5f5dc',
          80: 'rgba(245,245,220,0.80)',
          60: 'rgba(245,245,220,0.60)',
          40: 'rgba(245,245,220,0.40)',
          20: 'rgba(245,245,220,0.20)',
          10: 'rgba(245,245,220,0.10)',
        },
      },
      backgroundImage: {
        'crimson-gradient': 'linear-gradient(to right, #dc143c, #8b0000)',
        'crimson-hover':    'linear-gradient(to right, #ff1493, #dc143c)',
        'hero-glow':        'radial-gradient(ellipse at 20% 50%, rgba(139,0,0,0.20) 0%, transparent 60%), radial-gradient(ellipse at 80% 20%, rgba(0,212,255,0.10) 0%, transparent 60%)',
      },
    },
  },
  plugins: [],
}
