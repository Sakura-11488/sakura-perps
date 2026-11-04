import React from 'react';
import { ScrollView, Text } from 'react-native';
import { Stack } from 'expo-router';
import { useTheme } from '@/lib/theme';

export default function RoomsAvailability() {
  const { colors } = useTheme();
  return <ScrollView contentInsetAdjustmentBehavior="automatic" style={{ backgroundColor: colors.background }} contentContainerStyle={{ padding: 24, gap: 20 }}>
    <Stack.Screen options={{ headerShown: true, title: 'Sakura Rooms' }} />
    <Text selectable style={{ color: colors.primaryLight, fontSize: 12, fontWeight: '600' }}>LOCAL PAPER PILOT</Text>
    <Text selectable style={{ color: colors.text, fontSize: 28, fontWeight: '600' }}>A space to test your plan.</Text>
    <Text selectable style={{ color: colors.textSecondary, fontSize: 16, lineHeight: 26 }}>The Rooms pilot runs in the Sakura web app at /app/perps/rooms. Its paper engine and records stay in the browser. It is not available inside the native app yet.</Text>
    <Text selectable style={{ color: colors.textSecondary, fontSize: 16, lineHeight: 26 }}>Write a plan, approve a paper allocation, and review a fixed scenario. This pilot does not connect a real wallet, accept deposits, or place live trades.</Text>
  </ScrollView>;
}
