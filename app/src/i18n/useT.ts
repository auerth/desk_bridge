import { useCallback } from 'react';
import { useStore } from '../store';
import { translate, type MessageKey, type Params } from './index';

export type TFn = (key: MessageKey, params?: Params) => string;

/** Translation hook; re-renders when the language changes. */
export function useT(): TFn {
  const lang = useStore((s) => s.lang);
  return useCallback((key, params) => translate(lang, key, params), [lang]);
}

export function useLang() {
  return useStore((s) => s.lang);
}
