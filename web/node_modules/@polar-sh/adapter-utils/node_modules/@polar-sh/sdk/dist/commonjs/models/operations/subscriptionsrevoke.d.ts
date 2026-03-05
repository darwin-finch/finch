import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
export type SubscriptionsRevokeRequest = {
    /**
     * The subscription ID.
     */
    id: string;
};
/** @internal */
export declare const SubscriptionsRevokeRequest$inboundSchema: z.ZodType<SubscriptionsRevokeRequest, z.ZodTypeDef, unknown>;
/** @internal */
export type SubscriptionsRevokeRequest$Outbound = {
    id: string;
};
/** @internal */
export declare const SubscriptionsRevokeRequest$outboundSchema: z.ZodType<SubscriptionsRevokeRequest$Outbound, z.ZodTypeDef, SubscriptionsRevokeRequest>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace SubscriptionsRevokeRequest$ {
    /** @deprecated use `SubscriptionsRevokeRequest$inboundSchema` instead. */
    const inboundSchema: z.ZodType<SubscriptionsRevokeRequest, z.ZodTypeDef, unknown>;
    /** @deprecated use `SubscriptionsRevokeRequest$outboundSchema` instead. */
    const outboundSchema: z.ZodType<SubscriptionsRevokeRequest$Outbound, z.ZodTypeDef, SubscriptionsRevokeRequest>;
    /** @deprecated use `SubscriptionsRevokeRequest$Outbound` instead. */
    type Outbound = SubscriptionsRevokeRequest$Outbound;
}
export declare function subscriptionsRevokeRequestToJSON(subscriptionsRevokeRequest: SubscriptionsRevokeRequest): string;
export declare function subscriptionsRevokeRequestFromJSON(jsonString: string): SafeParseResult<SubscriptionsRevokeRequest, SDKValidationError>;
//# sourceMappingURL=subscriptionsrevoke.d.ts.map