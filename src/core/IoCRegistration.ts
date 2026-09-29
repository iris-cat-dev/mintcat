import { IoC } from '@/core/IoC.ts';
import { AppViewModel } from '@/AppViewModel';
import { TreeViewModel } from '@/pages/HomePage/TreeViewModel';
import { HomeViewModel } from '@/pages/HomePage/HomeViewModel';
import { ProfileViewModel } from '@/dialogs/ProfileEditDialog/ProfileViewModel';
import { AgentViewModel } from '@/components/AgentPanel/AgentViewModel';
import { ProfileService } from '@/services/ProfileService';
import {
    AuthResolver,
    NetworkClient,
    RequestLogger,
    RoutePolicy,
} from '@/services/network';
import { StorageAPI } from '@/storage';
import { EventDebugger } from '@/events/EventDebugger';

/**
 * Register services to DI container
 *
 * This function should be called early in the application lifecycle,
 * before any ViewModels are accessed.
 *
 * @example
 * ```typescript
 * // In App.tsx
 * React.useEffect(() => {
 *     registerIoC();
 *     AppInitializer.initializeCore();
 * }, []);
 * ```
 */
export function registerIoC(): void {
    IoC.register(
        StorageAPI,
        async () => new StorageAPI(),
        async (storage) => await storage.initDB()
    );

    IoC.register(
        EventDebugger,
        async () => new EventDebugger()
    );

    // Service layer - shared singleton
    IoC.register(
        ProfileService,
        async () => new ProfileService()
    );

    IoC.register(
        AuthResolver,
        async () => new AuthResolver()
    );

    IoC.register(
        RoutePolicy,
        async () => new RoutePolicy()
    );

    IoC.register(
        RequestLogger,
        async () => new RequestLogger()
    );

    IoC.register(
        NetworkClient,
        async () => new NetworkClient(
            await IoC.get(AuthResolver),
            await IoC.get(RoutePolicy),
            await IoC.get(RequestLogger),
        )
    );

    // Core ViewModel - shared across windows
    IoC.register(
        AppViewModel,
        async () => new AppViewModel(),
        async (vm) => await vm.initialize()
    );

    // UI ViewModels - per-window instances
    // Note: These are registered but not initialized until accessed
    IoC.register(
        TreeViewModel,
        async () => new TreeViewModel(),
        async (vm) => await vm.initialize()
    );

    IoC.register(
        HomeViewModel,
        async () => new HomeViewModel(),
        async (vm) => await vm.initialize()
    );
    IoC.register(
        AgentViewModel,
        async () => new AgentViewModel(),
        async (vm) => await vm.initialize()
    );


    IoC.register(
        ProfileViewModel,
        async () => new ProfileViewModel()
    );
}
