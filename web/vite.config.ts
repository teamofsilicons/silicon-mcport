import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'node:path';
export default defineConfig({plugins:[react()],resolve:{alias:{'@':path.resolve(import.meta.dirname,'src')}},server:{port:4381,strictPort:true,proxy:{'/api':{target:'http://127.0.0.1:4380',changeOrigin:true}}},build:{sourcemap:true}});
