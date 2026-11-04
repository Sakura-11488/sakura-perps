import React from 'react';
import { ScrollView, Text } from 'react-native';
import { Stack } from 'expo-router';
import { useTheme } from '@/lib/theme';

export default function CommunityRoomsAvailability() {
  const { colors } = useTheme();
  return <ScrollView contentInsetAdjustmentBehavior="automatic" style={{ backgroundColor: colors.background }} contentContainerStyle={{ padding: 24, gap: 20 }}>
    <Stack.Screen options={{ headerShown: true, title: 'Community Rooms' }} />
    <Text selectable style={{ color: colors.primaryLight, fontSize: 12, fontWeight: '600' }}>INVITE-ONLY PAPER BETA</Text>
    <Text selectable style={{ color: colors.text, fontSize: 28, fontWeight: '600' }}>A shared idea. Your own say.</Text>
    <Text selectable style={{ color: colors.textSecondary, fontSize: 16, lineHeight: 26 }}>Community Rooms runs in the Sakura web app at /app/perps/community. Join an invited paper room, review its fixed plan, and approve your own allocation with a browser-held paper identity.</Text>
    <Text selectable style={{ color: colors.textSecondary, fontSize: 16, lineHeight: 26 }}>The beta requires the community service. It does not connect a real wallet, accept deposits, or place live trades. Native participation is not available yet.</Text>
  </ScrollView>;
}
