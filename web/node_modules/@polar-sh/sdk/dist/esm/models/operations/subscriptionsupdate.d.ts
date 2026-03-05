import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SubscriptionUpdate, SubscriptionUpdate$Outbound } from "../components/subscriptionupdate.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type SubscriptionsUpdateRequest = {
    /**
     * The subscription ID.
     */
    id: string;
    subscriptionUpdate: SubscriptionUpdate;
};
/** @internal */
export declare const SubscriptionsUpdateRequest$inboundSchema: z.ZodType<SubscriptionsUpdateRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsUpdateRequest$Outbound = {
    id: string;
    SubscriptionUpdate: SubscriptionUpdate$Outbound;
};
/** @internal */
export declare const SubscriptionsUpdateRequest$outboundSchema: z.ZodType<SubscriptionsUpdateRequest$Outbound, z.ZodTypeDef, SubscriptionsUpdateRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsUpdateRequest$ {
    /** @deprecated use `SubscriptionsUpdateRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsUpdateRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsUpdateRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsUpdateRequest$Outbound, z.ZodTypeDef, SubscriptionsUpdateRequest>;
    /** @deprecated use `SubscriptionsUpdateRequest$Outbound` instead. */
    type Outbound = SubscriptionsUpdateRequest$Outbound;
}
export declare function subscriptionsUpdateRequestToJSON(subscriptionsUpdateRequest: SubscriptionsUpdateRequest): string;
export declare function subscriptionsUpdateRequestFromJSON(jsonString: string): SafeParseResult<SubscriptionsUpdateRequest, SDKValidationError>;
//# sourceMappingURL=subscriptionsupdate.d.ts.map