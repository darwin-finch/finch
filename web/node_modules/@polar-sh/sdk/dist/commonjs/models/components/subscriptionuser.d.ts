import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type SubscriptionUser = {
    id: string;
    email: string;
    publicName: string;
    avatarUrl?: string | null | undefined;
    githubUsername?: string | null | undefined;
};
/** @internal */
export declare const SubscriptionUser$inboundSchema: z.ZodType<SubscriptionUser, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionUser$Outbound = {
    id: string;
    email: string;
    public_name: string;
    avatar_url?: string | null | undefined;
    github_username?: string | null | undefined;
};
/** @internal */
export declare const SubscriptionUser$outboundSchema: z.ZodType<SubscriptionUser$Outbound, z.ZodTypeDef, SubscriptionUser>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionUser$ {
    /** @deprecated use `SubscriptionUser$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionUser, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionUser$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionUser$Outbound, z.ZodTypeDef, SubscriptionUser>;
    /** @deprecated use `SubscriptionUser$Outbound` instead. */
    type Outbound = SubscriptionUser$Outbound;
}
export declare function subscriptionUserToJSON(subscriptionUser: SubscriptionUser): string;
export declare function subscriptionUserFromJSON(jsonString: string): SafeParseResult<SubscriptionUser, SDKValidationError>;
//# sourceMappingURL=subscriptionuser.d.ts.map