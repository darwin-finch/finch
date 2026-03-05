import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { Pledge, Pledge$Outbound } from "./pledge.js";
/**
 * Sent when a pledge is updated.
 *
 * @remarks
 *
 * **Discord & Slack support:** Basic
 */
export type WebhookPledgeUpdatedPayload = {
    type?: "pledge.updated" | undefined;
    data: Pledge;
};
/** @internal */
export declare const WebhookPledgeUpdatedPayload$inboundSchema: z.ZodType<WebhookPledgeUpdatedPayload, z.ZodTypeDef, unknown>;
/** @internal */
export type WebhookPledgeUpdatedPayload$Outbound = {
    type: "pledge.updated";
    data: Pledge$Outbound;
};
/** @internal */
export declare const WebhookPledgeUpdatedPayload$outboundSchema: z.ZodType<WebhookPledgeUpdatedPayload$Outbound, z.ZodTypeDef, WebhookPledgeUpdatedPayload>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace WebhookPledgeUpdatedPayload$ {
    /** @deprecated use `WebhookPledgeUpdatedPayload$inboundSchema` instead. */
    const inboundSchema: z.ZodType<WebhookPledgeUpdatedPayload, z.ZodTypeDef, unknown>;
    /** @deprecated use `WebhookPledgeUpdatedPayload$outboundSchema` instead. */
    const outboundSchema: z.ZodType<WebhookPledgeUpdatedPayload$Outbound, z.ZodTypeDef, WebhookPledgeUpdatedPayload>;
    /** @deprecated use `WebhookPledgeUpdatedPayload$Outbound` instead. */
    type Outbound = WebhookPledgeUpdatedPayload$Outbound;
}
export declare function webhookPledgeUpdatedPayloadToJSON(webhookPledgeUpdatedPayload: WebhookPledgeUpdatedPayload): string;
export declare function webhookPledgeUpdatedPayloadFromJSON(jsonString: string): SafeParseResult<WebhookPledgeUpdatedPayload, SDKValidationError>;
//# sourceMappingURL=webhookpledgeupdatedpayload.d.ts.map