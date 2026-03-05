import * as z from "zod";
import { Result as SafeParseResult } from "../../types/fp.js";
import { SDKValidationError } from "../errors/sdkvalidationerror.js";
import { BenefitDiscordSubscriberProperties, BenefitDiscordSubscriberProperties$Outbound } from "./benefitdiscordsubscriberproperties.js";
import { Organization, Organization$Outbound } from "./organization.js";
export type BenefitDiscordSubscriber = {
    /**
     * Creation timestamp of the object.
     */
    createdAt: Date;
    /**
     * Last modification timestamp of the object.
     */
    modifiedAt: Date | null;
    /**
     * The ID of the benefit.
     */
    id: string;
    type?: "discord" | undefined;
    /**
     * The description of the benefit.
     */
    description: string;
    /**
     * Whether the benefit is selectable when creating a product.
     */
    selectable: boolean;
    /**
     * Whether the benefit is deletable.
     */
    deletable: boolean;
    /**
     * The ID of the organization owning the benefit.
     */
    organizationId: string;
    organization: Organization;
    /**
     * Properties available to subscribers for a benefit of type `discord`.
     */
    properties: BenefitDiscordSubscriberProperties;
};
/** @internal */
export declare const BenefitDiscordSubscriber$inboundSchema: z.ZodType<BenefitDiscordSubscriber, z.ZodTypeDef, unknown>;
/** @internal */
export type BenefitDiscordSubscriber$Outbound = {
    created_at: string;
    modified_at: string | null;
    id: string;
    type: "discord";
    description: string;
    selectable: boolean;
    deletable: boolean;
    organization_id: string;
    organization: Organization$Outbound;
    properties: BenefitDiscordSubscriberProperties$Outbound;
};
/** @internal */
export declare const BenefitDiscordSubscriber$outboundSchema: z.ZodType<BenefitDiscordSubscriber$Outbound, z.ZodTypeDef, BenefitDiscordSubscriber>;
/**
 * @internal
 * @deprecated This namespace will be removed in future versions. Use schemas and types that are exported directly from this module.
 */
export declare namespace BenefitDiscordSubscriber$ {
    /** @deprecated use `BenefitDiscordSubscriber$inboundSchema` instead. */
    const inboundSchema: z.ZodType<BenefitDiscordSubscriber, z.ZodTypeDef, unknown>;
    /** @deprecated use `BenefitDiscordSubscriber$outboundSchema` instead. */
    const outboundSchema: z.ZodType<BenefitDiscordSubscriber$Outbound, z.ZodTypeDef, BenefitDiscordSubscriber>;
    /** @deprecated use `BenefitDiscordSubscriber$Outbound` instead. */
    type Outbound = BenefitDiscordSubscriber$Outbound;
}
export declare function benefitDiscordSubscriberToJSON(benefitDiscordSubscriber: BenefitDiscordSubscriber): string;
export declare function benefitDiscordSubscriberFromJSON(jsonString: string): SafeParseResult<BenefitDiscordSubscriber, SDKValidationError>;
//# sourceMappingURL=benefitdiscordsubscriber.d.ts.map