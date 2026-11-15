import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import tailwindcss from '@tailwindcss/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		tailwindcss(),
		sveltekit({
			compilerOptions: {
				// runes everywhere except in dependencies; the option goes away with Svelte 6
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// every path that is not a file gets 200.html, which boots the SPA
			adapter: adapter({ fallback: '200.html', precompress: true }),
			// written as a <meta> in 200.html, with the hash of the inline boot script;
			// frame-ancestors is ignored in a <meta>, so the server sends it as a header
			csp: {
				mode: 'hash',
				directives: {
					'default-src': ['self'],
					'script-src': ['self'],
					// bits-ui positions popovers and Svelte transitions add <style> elements
					'style-src': ['self', 'unsafe-inline'],
					'object-src': ['none'],
					'base-uri': ['none'],
					'form-action': ['self']
				}
			}
		})
	],
	server: {
		// the browser talks to Vite only, so the API is on the same origin as in production
		proxy: { '^/t/[^/]+/api/': 'http://127.0.0.1:3000' }
	}
});
