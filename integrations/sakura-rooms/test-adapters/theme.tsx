// Test shell only. The Expo application supplies its real theme provider.
import { useState } from 'react';
export function useTheme() {
  const [isDark, setDark] = useState(false);
  return { isDark, toggleTheme: () => setDark(value => !value) };
}
